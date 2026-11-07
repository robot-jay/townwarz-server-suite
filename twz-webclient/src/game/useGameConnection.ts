import { useCallback, useEffect, useRef, useState } from 'react';
import { config } from '../config';
import { emptyInput, isSnapshot, type Snapshot } from './protocol';
import { isExistingGameSummary, isExpiredGameNotice, type ExistingGameSummary, type ExpiredGameNotice } from './domain';
import { isEconomyState, isConstructionReply, type EconomyState, type ConstructionIntent } from './construction';
import { ConstructionCommands, type ConstructionCommandsState } from './constructionCommands';

type Status = 'idle' | 'checking' | 'starting' | 'playing' | 'ended' | 'existing' | 'abandoning' | 'expired' | 'acknowledging';
type EntryMode = 'check' | 'start' | 'resume' | 'quit' | 'abandon' | 'ack-expiry';

export function useGameConnection(getIdentityToken: () => Promise<string>) {
  const snapshot = useRef<Snapshot | null>(null);
  const input = useRef(emptyInput());
  const socket = useRef<WebSocket | null>(null);
  const cancel = useRef<(() => void) | null>(null);
  const generation = useRef(0);
  const busy = useRef(false);
  const hasPlayed = useRef(false);
  const quitReply = useRef<((ok: boolean) => void) | null>(null);
  const quitBusy = useRef(false);
  const [quitting, setQuitting] = useState(false);
  const [status, setStatus] = useState<Status>('checking');
  const [error, setError] = useState('');
  const [existingGame, setExistingGame] = useState<ExistingGameSummary | null>(null);
  const existing = useRef<ExistingGameSummary | null>(null);
  const rememberExisting = useCallback((game: ExistingGameSummary | null) => {
    existing.current = game; setExistingGame(game);
  }, []);
  const [expiredGame, setExpiredGame] = useState<ExpiredGameNotice | null>(null);
  const expired = useRef<ExpiredGameNotice | null>(null);
  const rememberExpired = useCallback((notice: ExpiredGameNotice | null) => {
    expired.current = notice; setExpiredGame(notice);
  }, []);
  const [canResume, setCanResume] = useState(false);
  const [ping, setPing] = useState<number | null>(null);
  const [playerId, setPlayerId] = useState('');
  const [self, setSelf] = useState({ x: 0, y: 0, hp: 100, ready: false });
  const [cash, setCash] = useState<number | null>(null);
  const [catalogVersion, setCatalogVersion] = useState<number>();
  const [economy, setEconomy] = useState<EconomyState | null>(null);
  const latestEconomy = useRef<EconomyState | null>(null);
  const [constructionCommands, setConstructionCommands] = useState<ConstructionCommandsState>({ pending: null, notice: null });
  const commands = useRef<ConstructionCommands | null>(null);
  if (!commands.current) commands.current = new ConstructionCommands(setConstructionCommands);
  const clear = useCallback(() => {
    snapshot.current = null; input.current = emptyInput();
    commands.current?.disconnect(); latestEconomy.current = null; setEconomy(null); setCash(null); setCatalogVersion(undefined);
    setPing(null); setPlayerId(''); setSelf({ x: 0, y: 0, hp: 100, ready: false });
  }, []);
  const reset = useCallback(() => {
    generation.current++; cancel.current?.(); cancel.current = null; busy.current = false;
    commands.current?.endSession();
    clear(); rememberExisting(null); rememberExpired(null); hasPlayed.current = false; setError(''); setCanResume(false); setStatus('idle');
  }, [clear, rememberExisting, rememberExpired]);

  const connect = useCallback(async (mode: EntryMode, townName?: string, targetGameId?: string): Promise<boolean> => {
    if (busy.current) return false;
    busy.current = true;
    const currentGeneration = ++generation.current;
    clear(); setError(''); setStatus(mode === 'start' ? 'starting' : mode === 'abandon' ? 'abandoning' : mode === 'ack-expiry' ? 'acknowledging' : 'checking');
    const failureStatus = (): Status => (mode === 'ack-expiry' || mode === 'check') && expired.current
      ? 'expired' : (mode === 'abandon' || mode === 'check') && existing.current
        ? 'existing' : mode === 'start' ? 'idle' : 'ended';
    try {
      const token = await getIdentityToken();
      if (currentGeneration !== generation.current) return false;
      const ws = new WebSocket(config.gameUrl);
      socket.current = ws;
      return await new Promise<boolean>(resolve => {
        let stopped = false, entered = false, lastMessage = Date.now(), pingSent = 0, awaitingPong = false;
        const cleanup = () => {
          if (stopped) return;
          stopped = true; clearTimeout(deadline); clearInterval(send); clearInterval(heartbeat);
          ws.onopen = ws.onmessage = ws.onerror = ws.onclose = null;
          ws.close(); if (socket.current === ws) socket.current = null;
          resolve(false);
        };
        const finish = (message: string, next: Status, resumable: boolean, ok = false) => {
          if (stopped) return;
          resolve(ok); quitReply.current?.(ok); quitReply.current = null;
          if (!resumable) commands.current?.endSession();
          cleanup(); busy.current = false; clear(); setError(message); setStatus(next); setCanResume(resumable);
        };
        const deadline = setTimeout(() => finish('The game server did not finish the request. Please try again.', failureStatus(), true), 30000);
        const send = setInterval(() => {
          if (entered && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ v: 1, type: 'input', ...input.current, ts: Date.now() }));
        }, 50);
        const heartbeat = setInterval(() => {
          if (!entered || ws.readyState !== WebSocket.OPEN) return;
          if (Date.now() - lastMessage > 10000) {
            finish('Connection lost. Your town is reserved for 15 minutes. Reconnect to continue.', 'ended', true); return;
          }
          if (!awaitingPong || performance.now() - pingSent > 5000) {
            pingSent = performance.now(); awaitingPong = true; ws.send(JSON.stringify({ v: 1, type: 'ping' }));
          }
        }, 1000);
        cancel.current = cleanup;
        ws.onopen = () => ws.send(JSON.stringify(mode === 'start'
          ? { v: 1, type: 'start_game', id_token: token, role: 'mayor', town_name: townName }
          : mode === 'abandon'
            ? { v: 1, type: 'abandon_game', id_token: token, player_id: targetGameId }
            : mode === 'ack-expiry'
              ? { v: 1, type: 'acknowledge_expiry', id_token: token, player_id: targetGameId }
            : { v: 1, type: mode === 'quit' ? 'quit_game' : mode === 'check' ? 'check_game' : 'resume_game', id_token: token }));
        ws.onmessage = event => {
          lastMessage = Date.now();
          try {
            const message: unknown = JSON.parse(event.data);
            if (isSnapshot(message) && (mode === 'start' || mode === 'resume')) {
              snapshot.current = message; setPlayerId(message.me);
              setCatalogVersion(message.catalog_version);
              if (!latestEconomy.current && message.self_state) setCash(message.self_state.wallet.cash);
              const me = message.players.find(p => p[0] === message.me);
              if (me) {
                if (!entered) {
                  const town = message.towns?.find(town => town.mayor_player_id === message.me);
                  if (town) rememberExisting({ player_id: message.me, town_id: town.id, town_name: town.name,
                    is_connected: true, reconnect_until_ms: null });
                }
                rememberExpired(null);
                commands.current?.setSession(message.self_state?.identity.game_id ?? message.me, command => {
                  if (stopped || ws.readyState !== WebSocket.OPEN || quitBusy.current) return false;
                  ws.send(JSON.stringify({ v: 1, ...command })); return true;
                });
                entered = true; hasPlayed.current = true; clearTimeout(deadline); setCanResume(true); setStatus('playing');
                setSelf({ x: me[1], y: me[2], hp: me[4], ready: true }); resolve(true);
              }
            } else if (message && typeof message === 'object' && 'type' in message) {
              if (entered && isEconomyState(message)) {
                const ownGame = snapshot.current?.self_state?.identity.game_id ?? snapshot.current?.me;
                if (message.owned_buildings.every(record => record.building.ownership?.game_id === ownGame)
                  && (!latestEconomy.current || message.revision >= latestEconomy.current.revision)) {
                  latestEconomy.current = message; setEconomy(message); setCash(message.cash);
                }
              } else if (entered && isConstructionReply(message)) {
                commands.current?.receive(message);
              } else if (message.type === 'existing_game' && 'game' in message && isExistingGameSummary(message.game)) {
                rememberExpired(null); rememberExisting(message.game); finish('', 'existing', true, true);
              } else if (message.type === 'grace_expired' && 'notice' in message && isExpiredGameNotice(message.notice)) {
                rememberExisting(null); rememberExpired(message.notice); finish('', 'expired', false, true);
              } else if (message.type === 'expiry_notice_ack') {
                rememberExpired(null); finish('', 'idle', false, true);
              } else if (message.type === 'no_game') {
                rememberExisting(null); rememberExpired(null);
                finish(hasPlayed.current ? 'Your previous town is no longer available.' : '', hasPlayed.current ? 'ended' : 'idle', false, true);
              } else if (message.type === 'game_quit_ack') {
                rememberExisting(null); finish('', 'idle', false, true);
              }
              else if (message.type === 'game_ended') finish('This town is no longer available. Start a new game to return to the frontier.', 'ended', false);
              else if (message.type === 'game_error' && 'message' in message && typeof message.message === 'string') {
                if (quitReply.current) { setError(message.message); quitReply.current(false); quitReply.current = null; }
                else finish(message.message, failureStatus(), mode !== 'start');
              } else if (message.type === 'pong' && awaitingPong) {
                setPing(Math.round(performance.now() - pingSent)); awaitingPong = false;
              }
            }
          } catch { /* Ignore malformed frames; entry and heartbeat deadlines apply. */ }
        };
        ws.onerror = () => finish('Unable to reach the game server. Please try again.', failureStatus(), true);
        ws.onclose = () => finish(entered
          ? 'Disconnected. Your town is reserved for 15 minutes. Reconnect to continue.'
          : 'The request was interrupted. Please try again.', entered ? 'ended' : failureStatus(), entered || mode !== 'start');
      });
    } catch (error) {
      if (currentGeneration !== generation.current) return false;
      busy.current = false; setStatus(failureStatus()); setCanResume(mode !== 'start');
      setError(error instanceof Error ? error.message : 'Unable to verify your sign-in. Please sign in again.');
      return false;
    }
  }, [clear, getIdentityToken, rememberExisting, rememberExpired]);

  const connectRef = useRef(connect);
  connectRef.current = connect;
  useEffect(() => {
    // Defer the probe so Strict Mode's mount/cleanup check cannot claim two sessions.
    const timer = setTimeout(() => { void connectRef.current('check'); }, 0);
    return () => {
      clearTimeout(timer); generation.current++; cancel.current?.(); busy.current = false;
      snapshot.current = null; input.current = emptyInput(); quitReply.current?.(false); quitReply.current = null;
      commands.current?.dispose();
    };
  }, []);

  const quit = useCallback(async (): Promise<boolean> => {
    if (quitBusy.current) return false;
    quitBusy.current = true; setQuitting(true);
    try {
    const ws = socket.current;
    if (ws?.readyState === WebSocket.OPEN && status === 'playing') {
      return await new Promise(resolve => {
        const timeout = setTimeout(() => {
          quitReply.current = null; setError('Could not confirm leaving the game. Please try again.'); resolve(false);
        }, 10000);
        quitReply.current = ok => { clearTimeout(timeout); resolve(ok); };
        input.current = emptyInput(); ws.send(JSON.stringify({ v: 1, type: 'quit_game' }));
      });
    }
    if (busy.current) return false;
    return await connect('quit');
    } finally { quitBusy.current = false; setQuitting(false); }
  }, [connect, status]);

  const abandonGame = useCallback(async (): Promise<boolean> => {
    if (quitBusy.current || busy.current || !existing.current) return false;
    const targetGameId = existing.current.player_id;
    quitBusy.current = true; setQuitting(true);
    try { return await connect('abandon', undefined, targetGameId); }
    finally { quitBusy.current = false; setQuitting(false); }
  }, [connect]);

  const acknowledgeExpiry = useCallback(async (): Promise<boolean> => {
    if (quitBusy.current || busy.current || !expired.current) return false;
    const gameId = expired.current.player_id;
    quitBusy.current = true; setQuitting(true);
    try { return await connect('ack-expiry', undefined, gameId); }
    finally { quitBusy.current = false; setQuitting(false); }
  }, [connect]);

  const submitConstruction = useCallback((intent: ConstructionIntent) => {
    if (status !== 'playing' || quitBusy.current) return false;
    return commands.current?.submit(intent) ?? false;
  }, [status]);
  const retryConstruction = useCallback(() => {
    if (status !== 'playing' || quitBusy.current) return false;
    return commands.current?.retry() ?? false;
  }, [status]);

  return { snapshot, input, status, error, canResume, existingGame, expiredGame, quitting, ping, playerId, self,
    cash, economy, catalogVersion, constructionCommands, submitConstruction, retryConstruction,
    dismissConstructionNotice: () => commands.current?.dismissNotice(), reset, quit, abandonGame, acknowledgeExpiry,
    checkGame: () => connect('check'), startGame: (name: string) => connect('start', name), resumeGame: () => connect('resume') };
}
