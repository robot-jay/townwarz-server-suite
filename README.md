# This is the server softwares for the Townwarz game.

## Here's an overview of how the architecture is supposed to work:
- Many clients talk to the game gateway.
- The game gateway talks to many cell servers.
- Each cell server acts as an authoritative source for game state, and manages inputs and state updates for the clients, then retransmits the states to each client within its "cell".
- There is a conductor behind the scenes that decides which clients use which cell server. Eventually the conductor will also handle scaling the app by provisioning new resources and spawning new cell server instances.
- All of the interprocess communication happens via a NATS server.

## To run the app, you need to start all the various processes in order. That order is:
- Conductor
- NATS server
- Cell server A
- Cell server B
- Gateway

## The current commands used for local dev are:
```Conductor
cargo run -p twz-conductor -- --bind 0.0.0.0:7000
```
```NATS server
nats-server -p 4222
```
```Cell server A
cargo run -p twz-cellserver -- --id A --bind 127.0.0.1:3001 --conductor http://127.0.0.1:7000 --nats nats://127.0.0.1:4222
```
```Cell server B
cargo run -p twz-cellserver -- --id B --bind 127.0.0.1:3002 --conductor http://127.0.0.1:7000 --nats nats://127.0.0.1:4222
```
```Gateway
cargo run -p twz-gateway -- --bind 0.0.0.0:8081 --conductor http://127.0.0.1:7000 --nats nats://127.0.0.1:4222
```

Eventually I'll convert all that to a cleaner startup script, and the conductor itself will handle spawning the cell servers.
