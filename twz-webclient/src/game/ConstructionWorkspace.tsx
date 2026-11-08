import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { GameCanvas, type CanvasTool } from './GameCanvas';
import type { useGameConnection } from './useGameConnection';
import { useBuildingCatalog } from './useBuildingCatalog';
import { CONSTRUCTION_TYPES, catalogLock, formatCash, previewPlacement, refundFor, rejectionMessage,
  type ConstructibleType, type TilePoint } from './construction';

export function ConstructionWorkspace({ game, children }: { game: ReturnType<typeof useGameConnection>; children: ReactNode }) {
  const { catalog, error: catalogError, loading, retry } = useBuildingCatalog(game.catalogVersion);
  const [mode, setMode] = useState<CanvasTool['mode']>('play');
  const [buildingType, setBuildingType] = useState<ConstructibleType | null>(null);
  const [target, setTarget] = useState<TilePoint | null>(null);
  const [locked, setLocked] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [demolitionConfirm, setDemolitionConfirm] = useState(false);
  const workspace = useRef<HTMLDivElement>(null);
  const pending = game.constructionCommands.pending;
  const notice = game.constructionCommands.notice;
  const blocked = !!pending || game.quitting;
  const snapshot = game.snapshot.current;
  const economy = game.economy;
  const definition = catalog?.buildings.find(b => b.building_type === buildingType) ?? null;
  const selectedRecord = economy?.owned_buildings.find(r => r.building.id === selectedId);
  const selectedView = snapshot?.buildings?.find(b => b.id === selectedId);
  const selected = selectedRecord?.building ?? selectedView;
  const selectedDefinition = catalog?.buildings.find(b => b.building_type === selected?.building_type);
  const preview = definition && target && snapshot && economy && catalog ? previewPlacement(definition, target, snapshot, economy, catalog) : null;
  const actorReason = !economy || !snapshot?.construction ? 'Waiting for authoritative construction state…'
    : !snapshot.construction.can_construct ? rejectionMessage(snapshot.construction.reason) || 'Construction is unavailable.' : null;
  const demolitionReason = actorReason ?? (selected?.building_type === 'town_hall' ? 'Townhalls cannot be demolished.'
    : !selectedRecord ? 'Only your purchased infrastructure can be demolished.' : null);
  const refund = selectedRecord && economy ? refundFor(selectedRecord.price_paid, economy.demolition_refund_percent) : null;

  const focusWorld = useCallback(() => {
    requestAnimationFrame(() => {
      const canvas = workspace.current?.querySelector('canvas');
      canvas?.focus({ preventScroll: true });
      if (window.matchMedia('(max-width: 900px)').matches) canvas?.scrollIntoView({ block: 'nearest' });
    });
  }, []);
  const cancel = useCallback(() => {
    if (blocked) return;
    setMode('play'); setTarget(null); setLocked(false); setSelectedId(null); setDemolitionConfirm(false); focusWorld();
  }, [blocked, focusWorld]);
  const onTarget = useCallback((point: TilePoint, fix: boolean) => {
    if (blocked) return;
    setTarget(current => current?.x === point.x && current.y === point.y ? current : point);
    if (fix) setLocked(true);
  }, [blocked]);
  const onSelect = useCallback((id: string | null) => {
    if (blocked) return;
    setSelectedId(id); setDemolitionConfirm(false);
  }, [blocked]);
  const confirm = useCallback(() => {
    const current = game.snapshot.current;
    if (blocked || mode !== 'place' || !definition || !target || !current || !economy || !catalog) return;
    if (previewPlacement(definition, target, current, economy, catalog).state === 'invalid') return;
    setLocked(true);
    game.submitConstruction({ type: 'place_building', building_type: definition.building_type as ConstructibleType, origin_tile: target });
  }, [blocked, mode, definition, target, economy, catalog, game]);
  useEffect(() => {
    if (notice?.kind === 'success') {
      setMode('play'); setTarget(null); setLocked(false); setSelectedId(null); setDemolitionConfirm(false);
    }
  }, [notice]);

  const tool: CanvasTool = { mode, definition, target, locked, selectedId, economy, catalog, onTarget, onSelect, onCancel: cancel, onConfirm: confirm };
  const territory = economy?.territory;
  const me = snapshot?.players.find(p => p[0] === snapshot.me);
  const ownTown = snapshot?.towns?.find(town => town.mayor_player_id === game.playerId);
  return <div className="construction-layout" ref={workspace}>
    <div className="viewport">
      <GameCanvas snapshot={game.snapshot} input={game.input} tool={tool} />
      {children}
      {mode !== 'play' && <div className="tool-mode-label">{mode === 'place' ? 'PLACEMENT MODE' : 'INSPECTION MODE'} · FIRING DISABLED</div>}
    </div>
    <aside className="construction-panel" aria-label="Town construction" onKeyDown={event => {
      if (event.key === 'Escape' && mode !== 'play') { event.preventDefault(); event.stopPropagation(); cancel(); }
    }}>
      <div className="construction-heading"><div><span className="eyebrow">TOWN INFRASTRUCTURE</span><h2>Build your foothold.</h2></div></div>
      {notice && <div className={`construction-notice ${notice.kind}`} role={notice.kind === 'success' ? 'status' : 'alert'}>
        <p>{notice.text}</p>
        {pending?.phase === 'uncertain'
          ? <button className="secondary" disabled={game.quitting} onClick={game.retryConstruction}>Retry saved request</button>
          : !pending && <button className="text-button" onClick={game.dismissConstructionNotice}>Dismiss</button>}
      </div>}
      {pending?.phase === 'sending' && <p className="construction-wait" role="status"><span className="spinner" /> Waiting for server confirmation…</p>}
      {actorReason && !pending && <p className="construction-restriction" role="status">{actorReason}</p>}
      {loading && <p className="field-help" role="status">Loading server building catalog…</p>}
      {catalogError && <div className="construction-notice error" role="alert"><p>{catalogError}</p><button className="secondary" onClick={retry}>Retry catalog</button></div>}
      {catalog && <>
        <div className="building-catalog" aria-label="Building catalog">
          {CONSTRUCTION_TYPES.map(type => {
            const building = catalog.buildings.find(b => b.building_type === type);
            if (!building) return null;
            const lock = catalogLock(building, economy, snapshot, catalog);
            return <div className={`building-option ${mode === 'place' && buildingType === type ? 'active' : ''}`} key={type}>
              <button aria-label={`Place ${building.name}`} aria-describedby={`${type}-help`} disabled={!!lock || blocked}
                onClick={() => {
                  if (catalogLock(building, economy, game.snapshot.current, catalog)) return;
                  game.dismissConstructionNotice(); setBuildingType(type); setTarget(null); setLocked(false); setSelectedId(null);
                  setDemolitionConfirm(false); setMode('place'); focusWorld();
                }}>
                <span className={`building-glyph ${type}`} aria-hidden="true">{type === 'wall_research' ? '⌬' : '▥'}</span>
                <span><strong>{building.name}</strong><small>{building.geometry.footprint_tiles.x}×{building.geometry.footprint_tiles.y} reserved tiles</small></span>
                <span className="building-price">{formatCash(building.cost)}<small>CASH</small></span>
              </button>
              <p id={`${type}-help`} className={lock ? 'building-lock' : 'field-help'}>{lock ?? (type === 'wall_research'
                ? 'Required to purchase new Wall Factories.' : 'Infrastructure only. Wall production arrives in Phase 4.')}</p>
            </div>;
          })}
        </div>
        {mode === 'place' && definition && <section className="placement-controls" aria-label="Placement controls">
          <h3>Place {definition.name}</h3>
          <p className="field-help">Move the pointer over the world. Click to fix the grid anchor, then confirm. W/S moves and A/D turns; firing is disabled.</p>
          <p className={`placement-feedback ${preview?.state ?? 'unknown'}`} role="status">
            {preview ? `${preview.state === 'valid' ? '✓' : preview.state === 'invalid' ? '×' : '?'} ${preview.message}` : 'Choose a position in the world to preview the footprint.'}
          </p>
          {target && <p className="placement-anchor">Anchor tile {target.x}, {target.y} · {locked ? 'Fixed' : 'Following pointer'}</p>}
          <div className="construction-actions">
            <button className="primary" disabled={blocked || !preview || preview.state === 'invalid'} onClick={confirm}>Confirm placement</button>
            <button className="secondary" disabled={blocked} onClick={cancel}>Cancel placement</button>
          </div>
          {locked && <button className="text-button" disabled={blocked} onClick={() => { setLocked(false); focusWorld(); }}>Move anchor</button>}
          <p className="field-help">Enter confirms · Escape or right-click cancels. Cash changes only after the server commits.</p>
        </section>}
      </>}
      <section className="building-management" aria-label="Building management">
        <button className="secondary inspect-toggle" disabled={blocked} aria-pressed={mode === 'select'} onClick={() => {
          if (mode === 'select') { cancel(); return; }
          setMode('select'); setBuildingType(null); setTarget(null); setSelectedId(null); setDemolitionConfirm(false); focusWorld();
        }}>{mode === 'select' ? 'Return to play' : 'Inspect & demolish'}</button>
        {mode === 'select' && <>
          <p className="field-help">Click a building in the world, or select your infrastructure below. Firing is disabled.</p>
          {economy && <ul className="owned-buildings" aria-label="Your infrastructure">
            {economy.owned_buildings.map(record => <li key={record.building.id}><button className="secondary" disabled={blocked}
              aria-pressed={selectedId === record.building.id} onClick={() => onSelect(record.building.id)}>
              {catalog?.buildings.find(b => b.building_type === record.building.building_type)?.name ?? record.building.building_type.replaceAll('_', ' ')}
              <small>Tile {record.building.origin_tile.x}, {record.building.origin_tile.y}</small>
            </button></li>)}
          </ul>}
          {economy?.owned_buildings.length === 0 && <p className="field-help">No purchased infrastructure yet. Your townhall is protected.</p>}
          {selectedId && <div className="building-inspector">
            <h3>{selectedDefinition?.name ?? (selected ? selected.building_type.replaceAll('_', ' ') : 'Building unavailable')}</h3>
            {selected && <p className="field-help">Anchor tile {selected.origin_tile.x}, {selected.origin_tile.y}</p>}
            {selectedRecord && <p>Paid {formatCash(selectedRecord.price_paid)} · Refund <strong>{formatCash(refund ?? 0)}</strong> ({economy?.demolition_refund_percent}%)</p>}
            {selected?.building_type === 'wall_research' && <p className="field-help">Existing factories remain valid after research demolition. New factories will require research again.</p>}
            {demolitionReason && <p className="construction-restriction">{demolitionReason}</p>}
            {demolitionConfirm ? <>
              <p>Demolish this building for {formatCash(refund ?? 0)} cash?</p>
              <div className="construction-actions"><button className="danger-button" disabled={blocked || !!demolitionReason} onClick={() => {
                if (!selectedRecord || demolitionReason || blocked) return;
                game.submitConstruction({ type: 'demolish_building', building_id: selectedRecord.building.id });
              }}>Confirm demolition</button><button className="secondary" disabled={blocked} onClick={() => setDemolitionConfirm(false)}>Keep building</button></div>
            </> : <button className="danger-button" disabled={blocked || !!demolitionReason} onClick={() => setDemolitionConfirm(true)}>Demolish selected building</button>}
          </div>}
        </>}
      </section>
      {territory && <section className="territory-summary" aria-label="Town territory">
        <svg viewBox={`0 0 ${territory.size_tiles.x} ${territory.size_tiles.y}`} role="img" aria-label="Town territory map with your position and infrastructure">
          <rect x="0" y="0" width={territory.size_tiles.x} height={territory.size_tiles.y} fill="#1d3024" stroke="#dfbb78" strokeWidth="1" />
          {ownTown && <rect x={ownTown.hall.origin.x - territory.min_tile.x} y={ownTown.hall.origin.y - territory.min_tile.y} width="3" height="3" fill="#dfbb78" />}
          {economy?.owned_buildings.map(record => {
            const size = catalog?.buildings.find(b => b.building_type === record.building.building_type)?.geometry.footprint_tiles;
            return size && <rect key={record.building.id} x={record.building.origin_tile.x - territory.min_tile.x} y={record.building.origin_tile.y - territory.min_tile.y} width={size.x} height={size.y} fill="#9cc7cd" />;
          })}
          {me && <circle cx={Math.max(1, Math.min(territory.size_tiles.x - 1, me[1] / 48 - territory.min_tile.x))} cy={Math.max(1, Math.min(territory.size_tiles.y - 1, me[2] / 48 - territory.min_tile.y))} r="1" fill="#f7f4e9" stroke="#101b17" strokeWidth=".4" />}
        </svg>
        <div><strong>Your territory · {territory.size_tiles.x}×{territory.size_tiles.y}</strong><p>Tiles {territory.min_tile.x}–{territory.min_tile.x + territory.size_tiles.x - 1}, {territory.min_tile.y}–{territory.min_tile.y + territory.size_tiles.y - 1}</p><small>Gold boundaries appear in tool modes. Stay inside to manage your town.</small></div>
      </section>}
      <p className="construction-footnote">Prices and results come from the server. Other infrastructure branches arrive in later phases.</p>
    </aside>
  </div>;
}
