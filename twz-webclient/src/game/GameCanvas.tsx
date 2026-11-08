import { useEffect, useRef, type RefObject } from 'react';
import { emptyInput, type Input, type Snapshot } from './protocol';
import { previewPlacement, screenToTile, type BuildingCatalog, type BuildingDefinition, type EconomyState, type TilePoint } from './construction';

const TILE = 48;
const colors = ['#526847', '#315e72', '#747c78', '#cfb46c'];
const bindings: Record<string, keyof Input> = {
  KeyW: 'up', ArrowUp: 'up', KeyS: 'down', ArrowDown: 'down',
  KeyA: 'left', ArrowLeft: 'left', KeyD: 'right', ArrowRight: 'right', Space: 'shoot',
};

export interface CanvasTool {
  mode: 'play' | 'place' | 'select'; definition: BuildingDefinition | null; target: TilePoint | null;
  locked: boolean; selectedId: string | null; economy: EconomyState | null; catalog: BuildingCatalog | null;
  onTarget: (point: TilePoint, locked: boolean) => void; onSelect: (id: string | null) => void;
  onCancel: () => void; onConfirm: () => void;
}
export function GameCanvas({ snapshot, input, tool }: {
  snapshot: RefObject<Snapshot | null>; input: RefObject<Input>; tool?: CanvasTool;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const toolRef = useRef(tool);
  toolRef.current = tool;
  const releaseRef = useRef<() => void>(() => {});
  useEffect(() => { releaseRef.current(); input.current = emptyInput(); }, [tool?.mode, input]);
  useEffect(() => {
    const canvas = canvasRef.current!;
    const ctx = canvas.getContext('2d')!;
    let width = 0, height = 0, frame = 0;
    const held = new Set<string>();
    let mouseDown = false;
    let pointer: { x: number; y: number } | null = null;
    let cancelledByRightClick = false;
    const update = () => {
      const next = emptyInput();
      held.forEach(code => { if (bindings[code]) next[bindings[code]] = true; });
      next.shoot = (toolRef.current?.mode ?? 'play') === 'play' && (next.shoot || mouseDown);
      input.current = next;
    };
    const release = () => { held.clear(); mouseDown = false; update(); };
    releaseRef.current = release;
    const keydown = (e: KeyboardEvent) => {
      if (document.activeElement !== canvas) return;
      if (e.code === 'Escape' && toolRef.current?.mode !== 'play') { e.preventDefault(); release(); toolRef.current?.onCancel(); return; }
      if (e.code === 'Enter' && toolRef.current?.mode === 'place') { e.preventDefault(); if (!e.repeat) toolRef.current.onConfirm(); return; }
      if (!bindings[e.code] || document.activeElement !== canvas) return;
      if (e.code === 'Space' && e.repeat) return;
      e.preventDefault(); held.add(e.code); update();
    };
    const keyup = (e: KeyboardEvent) => { held.delete(e.code); update(); };
    const down = (e: PointerEvent) => {
      canvas.focus();
      const active = toolRef.current;
      if (active && active.mode !== 'play') {
        mouseDown = false; update();
        if (e.button === 2) { cancelledByRightClick = true; e.preventDefault(); active.onCancel(); return; }
        if (e.button !== 0) return;
        const rect = canvas.getBoundingClientRect();
        const state = snapshot.current, me = state?.players.find(p => p[0] === state.me);
        if (!state || !me) return;
        const point = screenToTile(e.clientX - rect.left, e.clientY - rect.top, width, height, me[1], me[2]);
        if (active.mode === 'place') active.onTarget(point, true);
        else {
          const building = state.buildings?.find(b => point.x >= b.origin_tile.x && point.y >= b.origin_tile.y
            && point.x < b.origin_tile.x + b.geometry.footprint_tiles.x && point.y < b.origin_tile.y + b.geometry.footprint_tiles.y);
          active.onSelect(building?.id ?? null);
        }
        return;
      }
      if (e.button === 0) { mouseDown = true; update(); }
    };
    const move = (e: PointerEvent) => { const rect = canvas.getBoundingClientRect(); pointer = { x: e.clientX - rect.left, y: e.clientY - rect.top }; };
    const leave = () => { pointer = null; };
    const context = (e: MouseEvent) => { if (cancelledByRightClick || toolRef.current?.mode !== 'play') e.preventDefault(); cancelledByRightClick = false; };
    const up = () => { mouseDown = false; update(); };
    const visibility = () => { if (document.hidden) release(); };
    const resize = () => {
      const rect = canvas.getBoundingClientRect();
      width = rect.width; height = rect.height;
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.round(width * dpr); canvas.height = Math.round(height * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    };
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    resize();
    window.addEventListener('keydown', keydown);
    window.addEventListener('keyup', keyup);
    window.addEventListener('blur', release);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
    document.addEventListener('visibilitychange', visibility);
    canvas.addEventListener('blur', release);
    canvas.addEventListener('pointerdown', down);
    canvas.addEventListener('pointermove', move);
    canvas.addEventListener('pointerleave', leave);
    canvas.addEventListener('contextmenu', context);
    canvas.focus();

    const draw = () => {
      ctx.fillStyle = '#19251f'; ctx.fillRect(0, 0, width, height);
      const state = snapshot.current;
      const me = state?.players.find(p => p[0] === state.me);
      if (state && me) {
        // Fit inside the server's fixed 26×18 terrain window on every screen.
        const scale = Math.min(width / (24 * TILE), height / (16 * TILE));
        const sx = (x: number) => width / 2 + (x - me[1]) * scale;
        const sy = (y: number) => height / 2 + (y - me[2]) * scale;
        const size = TILE * scale;
        const active = toolRef.current;
        if (active?.mode === 'place' && pointer && !active.locked) {
          const target = screenToTile(pointer.x, pointer.y, width, height, me[1], me[2]);
          if (target.x !== active.target?.x || target.y !== active.target?.y) active.onTarget(target, false);
        }
        const tiles = state.tiles;
        if (tiles) for (let j = 0; j < tiles.h; j++) for (let i = 0; i < tiles.w; i++) {
          const tile = tiles.data[j * tiles.w + i];
          const x = sx((tiles.ox + i) * TILE), y = sy((tiles.oy + j) * TILE);
          ctx.fillStyle = colors[tile] || colors[0];
          ctx.fillRect(x, y, size + 0.5, size + 0.5);
          ctx.strokeStyle = '#0000000d'; ctx.strokeRect(x, y, size, size);
          if (tile === 2) {
            ctx.fillStyle = '#9aa29a'; ctx.fillRect(x + size * .18, y + size * .18, size * .6, size * .5);
          } else if (tile === 3 && state.buildings === undefined) {
            ctx.fillStyle = '#594b32'; ctx.fillRect(x + size * .18, y + size * .4, size * .64, size * .48);
            ctx.fillStyle = '#f4db9e'; ctx.beginPath();
            ctx.moveTo(x + size * .1, y + size * .45); ctx.lineTo(x + size * .5, y + size * .1);
            ctx.lineTo(x + size * .9, y + size * .45); ctx.fill();
            ctx.fillStyle = '#302e24'; ctx.fillRect(x + size * .43, y + size * .62, size * .18, size * .26);
          }
        }
        if (active && active.mode !== 'play' && active.economy) {
          const territory = active.economy.territory;
          const x = sx(territory.min_tile.x * TILE), y = sy(territory.min_tile.y * TILE);
          const w = territory.size_tiles.x * size, h = territory.size_tiles.y * size;
          ctx.save(); ctx.fillStyle = '#dfbb7809'; ctx.fillRect(x, y, w, h);
          ctx.strokeStyle = '#e7c78d'; ctx.lineWidth = 3; ctx.setLineDash([10, 5]); ctx.strokeRect(x, y, w, h);
          ctx.setLineDash([]); ctx.lineWidth = 1; ctx.strokeStyle = '#dfbb7830';
          for (let i = 1; i < territory.size_tiles.x; i++) { ctx.beginPath(); ctx.moveTo(x + i * size, y); ctx.lineTo(x + i * size, y + h); ctx.stroke(); }
          for (let i = 1; i < territory.size_tiles.y; i++) { ctx.beginPath(); ctx.moveTo(x, y + i * size); ctx.lineTo(x + w, y + i * size); ctx.stroke(); }
          ctx.restore();
        }
        for (const building of [...(state.buildings ?? [])].sort((a,b) => a.origin_tile.y-b.origin_tile.y)) {
          const x = sx(building.origin_tile.x * TILE), y = sy(building.origin_tile.y * TILE);
          const body = building.geometry.collision_px;
          const footprint = building.geometry.footprint_tiles;
          const w = footprint.x * size, h = footprint.y * size, bodyH = body.height * scale;
          ctx.fillStyle = '#839076'; ctx.fillRect(x,y + bodyH,w,h - bodyH);
          ctx.strokeStyle = '#263c3344'; ctx.lineWidth = 1;
          for (let i = 0; i <= footprint.x; i++) {
            ctx.beginPath(); ctx.moveTo(x + i * size,y + bodyH); ctx.lineTo(x + i * size,y + h); ctx.stroke();
          }
          const factory = building.building_type.endsWith('_factory');
          const research = building.building_type.endsWith('_research');
          ctx.fillStyle = factory ? '#344e57' : research ? '#425443' : '#594b32'; ctx.fillRect(x,y,w,bodyH);
          ctx.fillStyle = factory ? '#9ab2ae' : research ? '#b2bf94' : '#c7af76'; ctx.fillRect(x + 4 * scale,y + bodyH * .3,w - 8 * scale,bodyH * .7);
          ctx.fillStyle = '#f4db9e'; ctx.beginPath(); ctx.moveTo(x,y + bodyH * .4);
          ctx.lineTo(x + w / 2,y); ctx.lineTo(x + w,y + bodyH * .4); ctx.closePath(); ctx.fill();
          ctx.fillStyle = '#302e24'; ctx.fillRect(x + w * .43,y + bodyH * .65,w * .14,bodyH * .35);
          if (active?.selectedId === building.id) {
            ctx.save(); ctx.strokeStyle = '#8cdef0'; ctx.lineWidth = 3; ctx.strokeRect(x, y, w, h); ctx.restore();
          }
          if (active?.mode !== 'play' && active?.catalog && (factory || research)) {
            ctx.save(); ctx.font = '600 11px "DM Sans", sans-serif'; ctx.textAlign = 'center'; ctx.fillStyle = '#f1ecd9';
            ctx.fillText(active.catalog.buildings.find(b => b.building_type === building.building_type)?.name ?? building.building_type, x + w / 2, y - 7); ctx.restore();
          }
          if (building.town_name) {
            ctx.save(); ctx.font = '600 13px "DM Sans", sans-serif'; ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
            const labelWidth = ctx.measureText(building.town_name).width + 20;
            const labelX = x + w / 2, labelY = y - 16;
            ctx.fillStyle = '#102017e8'; ctx.fillRect(labelX-labelWidth/2,labelY-13,labelWidth,26);
            ctx.strokeStyle = building.ownership?.owner_player_id === state.me ? '#dfbb7888' : '#a1b58e66';
            ctx.strokeRect(labelX-labelWidth/2,labelY-13,labelWidth,26);
            ctx.fillStyle = '#f4db9e'; ctx.fillText(building.town_name,labelX,labelY); ctx.restore();
          }
        }
        for (const town of state.buildings === undefined ? (state.towns ?? []) : []) {
          const x = sx((town.hall.origin.x + .5) * TILE);
          const y = sy(town.hall.origin.y * TILE) - 9;
          if (x < -200 || x > width + 200 || y < -40 || y > height + 40) continue;
          ctx.save();
          ctx.font = '600 13px "DM Sans", sans-serif';
          ctx.textAlign = 'center';
          ctx.textBaseline = 'middle';
          const labelWidth = ctx.measureText(town.name).width + 20;
          ctx.fillStyle = '#102017e8';
          ctx.fillRect(x - labelWidth / 2, y - 13, labelWidth, 26);
          ctx.strokeStyle = town.mayor_player_id === state.me ? '#dfbb7888' : '#a1b58e66';
          ctx.strokeRect(x - labelWidth / 2, y - 13, labelWidth, 26);
          ctx.fillStyle = town.mayor_player_id === state.me ? '#f4db9e' : '#d3dfca';
          ctx.fillText(town.name, x, y);
          ctx.restore();
        }
        if (active?.mode === 'place' && active.definition && active.target && active.economy && active.catalog) {
          const preview = previewPlacement(active.definition, active.target, state, active.economy, active.catalog);
          const color = preview.state === 'valid' ? '#a5db94' : preview.state === 'invalid' ? '#f09280' : '#edc776';
          const x = sx(active.target.x * TILE), y = sy(active.target.y * TILE);
          const geometry = active.definition.geometry, footprint = geometry.footprint_tiles, collider = geometry.collision_px;
          ctx.save(); ctx.fillStyle = color + '30'; ctx.fillRect(x, y, footprint.x * size, footprint.y * size);
          ctx.fillStyle = color + '60'; ctx.fillRect(x + collider.x * scale, y + collider.y * scale, collider.width * scale, collider.height * scale);
          ctx.strokeStyle = color; ctx.lineWidth = 2; ctx.setLineDash(active.locked ? [] : [6, 4]);
          ctx.strokeRect(x, y, footprint.x * size, footprint.y * size);
          ctx.font = '700 11px "DM Sans", sans-serif'; ctx.fillStyle = color; ctx.textAlign = 'left';
          ctx.fillText(`${preview.state === 'valid' ? '✓' : preview.state === 'invalid' ? '×' : '?'} ${active.definition.name}`, x, y - 9); ctx.restore();
        }
        for (const [id, x, y, angle, hp] of state.players) {
          ctx.save(); ctx.translate(sx(x), sy(y)); ctx.rotate(angle); ctx.scale(scale, scale);
          ctx.globalAlpha = hp <= 0 ? .3 : 1;
          ctx.shadowColor = '#0008'; ctx.shadowBlur = 8;
          ctx.fillStyle = id === state.me ? '#f4e3ad' : '#c8e0de';
          ctx.beginPath(); ctx.moveTo(19, 0); ctx.lineTo(-12, -12); ctx.lineTo(-7, 0); ctx.lineTo(-12, 12); ctx.closePath(); ctx.fill();
          ctx.strokeStyle = '#25362c'; ctx.lineWidth = 2; ctx.stroke(); ctx.restore();
          if (id !== state.me && hp > 0) {
            ctx.fillStyle = '#17231d'; ctx.fillRect(sx(x) - 16 * scale, sy(y) - 23 * scale, 32 * scale, 3 * scale);
            ctx.fillStyle = '#a1c49c'; ctx.fillRect(sx(x) - 16 * scale, sy(y) - 23 * scale, 32 * scale * Math.max(0, hp) / 100, 3 * scale);
          }
        }
        ctx.fillStyle = '#ffcc8a';
        for (const b of state.bullets) { ctx.beginPath(); ctx.arc(sx(b.x), sy(b.y), Math.max(2, 3 * scale), 0, Math.PI * 2); ctx.fill(); }
      }
      frame = requestAnimationFrame(draw);
    };
    frame = requestAnimationFrame(draw);
    return () => {
      cancelAnimationFrame(frame); observer.disconnect(); release();
      window.removeEventListener('keydown', keydown); window.removeEventListener('keyup', keyup);
      window.removeEventListener('blur', release); window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up); document.removeEventListener('visibilitychange', visibility);
      canvas.removeEventListener('blur', release); canvas.removeEventListener('pointerdown', down);
      canvas.removeEventListener('pointermove', move); canvas.removeEventListener('pointerleave', leave);
      canvas.removeEventListener('contextmenu', context); releaseRef.current = () => {};
    };
  }, [snapshot, input]);
  return <canvas ref={canvasRef} className={tool?.mode && tool.mode !== 'play' ? 'construction-canvas' : ''} tabIndex={0}
    aria-label={tool?.mode === 'place' ? 'Building placement. Move pointer to preview, click to fix the anchor, Enter to confirm, Escape to cancel. Firing is disabled.'
      : tool?.mode === 'select' ? 'Building selection. Click a building to inspect it. Escape returns to play. Firing is disabled.'
        : 'TownWarz game world. W and S move, A and D turn, Space or left mouse fires. Click to focus.'} />;
}
