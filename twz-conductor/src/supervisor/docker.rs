use super::{ConductorCfg, ServiceCfg};
use anyhow::{Context, Result};
use bollard::{
    container::{
        Config, CreateContainerOptions, ListContainersOptions, StartContainerOptions,
    },
    models::{HostConfig, PortBinding},
    network::{CreateNetworkOptions, InspectNetworkOptions},
    Docker, API_DEFAULT_VERSION,
};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;

fn build_docker_client() -> Result<Docker> {
    // Prefer explicit connectors. This avoids DOCKER_HOST parsing quirks.
    #[cfg(unix)]
    {
        // Works inside your Linux container where /var/run/docker.sock is mounted
        Ok(Docker::connect_with_unix("/var/run/docker.sock", 120, API_DEFAULT_VERSION)
            .context("connect to docker via unix socket")?)
    }

    #[cfg(windows)]
    {
        // Works when you run the conductor natively on Windows during dev
        Ok(Docker::connect_with_named_pipe(r"//./pipe/docker_engine", 120, API_DEFAULT_VERSION)
            .context("connect to docker via named pipe")?)
    }
}

pub async fn run(cfg: ConductorCfg, interval: Duration) -> Result<()> {
    let docker = build_docker_client()?;
    ensure_network(&docker, &cfg.network).await?;

    ensure_service(&docker, &cfg.network, &cfg.nats).await?;
    wait_tcp("twz-nats", cfg.nats.port.unwrap_or(4222), Duration::from_secs(10)).await.ok();

    ensure_service(&docker, &cfg.network, &cfg.gateway).await?;
    for s in &cfg.cellservers {
        ensure_service(&docker, &cfg.network, s).await.ok();
    }

    loop {
        ensure_service(&docker, &cfg.network, &cfg.nats).await.ok();
        ensure_service(&docker, &cfg.network, &cfg.gateway).await.ok();
        for s in &cfg.cellservers {
            ensure_service(&docker, &cfg.network, s).await.ok();
        }
        sleep(interval).await;
    }
}


async fn ensure_network(docker: &Docker, net: &str) -> Result<()> {
    // Simpler: just pass None for options
    if docker.inspect_network(net, None::<InspectNetworkOptions<&str>>).await.is_ok() {
        return Ok(());
    }
    docker.create_network(CreateNetworkOptions {
        name: net,
        check_duplicate: true,
        driver: "bridge",
        ..Default::default()
    }).await?;
    Ok(())
}

async fn ensure_service(docker: &Docker, network: &str, svc: &ServiceCfg) -> Result<()> {
    // Find by name (existing or stopped)
    let filters = HashMap::from([("name", vec![svc.name.as_str()])]);
    let listed = docker.list_containers(Some(ListContainersOptions {
        all: true,
        filters,
        ..Default::default()
    })).await?;

    if let Some(existing) = listed.into_iter().find(|c| {
        c.names
            .as_ref()
            .map(|n| n.iter().any(|x| x.contains(&svc.name)))
            .unwrap_or(false)
    }) {
        if existing.state.as_deref() != Some("running") {
            docker.start_container(existing.id.as_deref().unwrap(), None::<StartContainerOptions<String>>).await?;
        }
        return Ok(());
    }

    // Pull remote images when not using :local tags
    if !svc.image.ends_with(":local") {
        use bollard::image::CreateImageOptions;
        let mut s = docker.create_image(
            Some(CreateImageOptions { from_image: svc.image.as_str(), ..Default::default() }),
            None,
            None,
        );
        while let Some(_p) = s.next().await {}
    }

    // HostConfig with port bindings and network
    let mut host_config = HostConfig::default();
    if let Some(port) = svc.port {
        let key = format!("{}/tcp", port);
        let mut pb = HashMap::new();
        pb.insert(
            key,
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(port.to_string()),
            }]),
        );
        host_config.port_bindings = Some(pb);
    }
    host_config.network_mode = Some(network.to_string());

    let cfg = Config {
        image: Some(svc.image.as_str()),
        env: svc.env.as_ref().map(|v| v.iter().map(|s| s.as_str()).collect()),
        // Exposed ports: optional. Docker honors port_bindings without this.
        exposed_ports: None,
        host_config: Some(host_config),
        labels: Some(HashMap::from([("twz.role".into(), "managed".into())])),
        ..Default::default()
    };

    // Bollard 0.16 wants `platform` in CreateContainerOptions; pass None.
    docker.create_container(
        Some(CreateContainerOptions::<&str> { name: svc.name.as_str(), platform: None }),
        cfg,
    ).await?;
    docker.start_container(svc.name.as_str(), None::<StartContainerOptions<String>>).await?;
    Ok(())
}

async fn wait_tcp(host: &str, port: u16, timeout: Duration) -> Result<()> {
    use tokio::net::TcpStream;
    let start = std::time::Instant::now();
    loop {
        if TcpStream::connect((host, port)).await.is_ok() { return Ok(()); }
        if start.elapsed() > timeout { anyhow::bail!("timeout waiting for {host}:{port}"); }
        sleep(Duration::from_millis(200)).await;
    }
}