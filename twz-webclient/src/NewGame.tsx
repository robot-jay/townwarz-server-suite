import { useCallback, useState, type FormEvent } from 'react';
import { useAuth0 } from '@auth0/auth0-react';
import { Brand, Shell } from './Layout';
import { GameCanvas } from './game/GameCanvas';
import { useGameConnection } from './game/useGameConnection';
import type { ExpiredGameNotice } from './game/domain';

export function NewGame() {
  const { user, logout, getIdTokenClaims, getAccessTokenSilently } = useAuth0();
  const [step, setStep] = useState<'role' | 'town'>('role');
  const [townName, setTownName] = useState('');
  const [validation, setValidation] = useState('');
  const token = useCallback(async () => {
    await getAccessTokenSilently();
    const claims = await getIdTokenClaims();
    if (!claims?.__raw) throw new Error('Please sign out and sign in again to verify your identity.');
    return claims.__raw;
  }, [getIdTokenClaims, getAccessTokenSilently]);
  const game = useGameConnection(token);
  const signOut = async () => {
    await game.quit();
    game.reset();
    await logout({ logoutParams: { returnTo: window.location.origin } });
  };
  const freshGame = () => {
    game.reset();
    setStep('role');
    setTownName('');
    setValidation('');
  };
  const leaveGame = async () => {
    const left = game.status !== 'playing' && game.existingGame ? await game.abandonGame() : await game.quit();
    if (left) freshGame();
  };
  const continueAfterExpiry = async () => { if (await game.acknowledgeExpiry()) freshGame(); };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (game.status === 'starting') return;
    const name = townName.trim();
    if (Array.from(name).length < 2 || Array.from(name).length > 32) {
      setValidation('Choose a town name between 2 and 32 characters.');
      return;
    }
    if (!/^[\p{L}\p{N} '-]+$/u.test(name)) {
      setValidation('Use letters, numbers, spaces, apostrophes, or hyphens.');
      return;
    }
    setValidation('');
    void game.startGame(name);
  };

  if (game.status === 'playing') {
    return <Game game={game} fallbackTownName={townName.trim()} onLeave={leaveGame} onSignOut={signOut} />;
  }

  return <Shell>
    <main className="onboarding">
      <div className="onboarding-heading">
        <span className="eyebrow">A NEW CHAPTER</span>
        <span className="muted">Welcome, {user?.given_name || user?.name || 'pioneer'} <button className="text-button" disabled={game.quitting || game.status === 'checking' || game.status === 'starting'} onClick={signOut}>Sign out</button></span>
      </div>
      {(game.status === 'expired' || game.status === 'acknowledging') && game.expiredGame ? <ExpiredGamePanel notice={game.expiredGame} busy={game.quitting} error={game.error} onContinue={continueAfterExpiry} onCheck={() => void game.checkGame()} /> : game.status === 'abandoning' ? <section className="panel ended-game" role="status"><span className="spinner" /><h1>Leaving your town…</h1><p>Waiting for the server to confirm abandonment.</p></section> : game.status === 'checking' ? <section className="panel ended-game" role="status"><span className="spinner" /><h1>Checking your frontier…</h1><p>Looking for your existing town.</p></section> : game.status === 'existing' && game.existingGame ? <section className="panel ended-game">
        <span className="eyebrow">TOWN IN PROGRESS</span>
        <h1>You already have a town.</h1>
        <h2 className="existing-town-name">{game.existingGame.town_name}</h2>
        <p>{game.existingGame.is_connected
          ? 'This town is active in another connection, or its previous connection is still closing.'
          : 'Your town is reserved. You can resume it or abandon it and start over.'}</p>
        {game.existingGame.reconnect_until_ms !== null && <p className="field-help">Reserved until {new Date(game.existingGame.reconnect_until_ms).toLocaleTimeString()}.</p>}
        {game.error && <div className="error" role="alert">{game.error}</div>}
        <div className="setup-actions">
          <button className="secondary" disabled={game.quitting} onClick={() => void game.checkGame()}>Check again</button>
          {!game.existingGame.is_connected && <button className="primary" disabled={game.quitting} onClick={() => void game.resumeGame()}>Resume town <span>↗</span></button>}
        </div>
        <button className="text-button" disabled={game.quitting} onClick={leaveGame}>Abandon town & start over</button>
        <p className="field-help">Abandoning ends your participation in this town. You cannot resume this game afterward.</p>
      </section> : game.status === 'ended' ? <section className="panel ended-game">
        <span className="eyebrow">SESSION ENDED</span>
        <h1>{game.canResume ? 'Your frontier awaits.' : 'A fresh start awaits.'}</h1>
        <p role="status">{game.error}</p>
        {game.canResume ? <><button className="primary" disabled={game.quitting} onClick={() => void game.resumeGame()}>Reconnect <span>↗</span></button><button className="text-button" disabled={game.quitting} onClick={leaveGame}>Abandon town & start over</button></> : <button className="primary" onClick={freshGame}>Start a new game <span>↗</span></button>}
      </section> : <>
        <ol className="setup-steps" aria-label="New game progress">
          <li aria-current={step === 'role' ? 'step' : undefined}><span>01</span> CHOOSE YOUR ROLE</li>
          <li aria-current={step === 'town' ? 'step' : undefined}><span>02</span> FOUND YOUR TOWN</li>
          <li><span>03</span> ENTER THE FRONTIER</li>
        </ol>
        {step === 'role' ? <section className="role-selection">
          <h1>How will you make your mark?</h1>
          <p className="muted">Choose your role for this game.</p>
          <div className="role-grid">
            <button className="role-card mayor" onClick={() => setStep('town')}>
              <span className="role-icon" aria-hidden="true">♜</span>
              <span className="eyebrow">BUILD YOUR ROOTS</span>
              <strong>Mayor</strong>
              <span>Found a town, give it a name, and stake your claim in the frontier.</span>
              <span className="role-action">Become a mayor <span>↗</span></span>
            </button>
            <button className="role-card" disabled>
              <span className="role-icon" aria-hidden="true">⌖</span>
              <span className="eyebrow">TAKE THE FIGHT FURTHER</span>
              <strong>Commando</strong>
              <span>A different way to make your mark. This role is still in development.</span>
              <span className="badge">COMING SOON</span>
            </button>
          </div>
          <p className="new-game-note">Unexpected disconnects reserve your town for 15 minutes. Leaving or signing out releases it.</p>
        </section> : <section className="town-setup panel">
          <span className="eyebrow">YOUR ROLE / MAYOR</span>
          <h1>Give your town a name.</h1>
          <p className="muted">The name you choose will appear above your townhall for everyone in the world to see.</p>
          <form onSubmit={submit}>
            <label htmlFor="town-name">Town name</label>
            <input id="town-name" autoFocus autoComplete="off" value={townName} disabled={game.status === 'starting'} onChange={e => { setTownName(e.target.value); setValidation(''); }} maxLength={64} placeholder="e.g. Copper Creek" aria-describedby="town-name-help town-name-error" aria-invalid={Boolean(validation)} />
            <p id="town-name-help" className="field-help">2–32 characters · Letters, numbers, spaces, apostrophes, and hyphens</p>
            {(validation || game.error) && <div id="town-name-error" className="error" role="alert">{validation || game.error}</div>}
            <div className="setup-actions">
              <button type="button" className="secondary" disabled={game.status === 'starting'} onClick={() => { game.reset(); setStep('role'); }}>Back</button>
              <button className="primary" disabled={game.status === 'starting'}>{game.status === 'starting' ? 'Founding your town…' : 'Found town & enter world'} <span>{game.status === 'starting' ? '…' : '↗'}</span></button>
            </div>
            {game.status === 'starting' && <p className="field-help" role="status">Verifying your identity and preparing your new game…</p>}
          </form>
        </section>}
      </>}
    </main>
  </Shell>;
}

function ExpiredGamePanel({ notice, busy, error, onContinue, onCheck }: {
  notice: ExpiredGameNotice; busy: boolean; error: string; onContinue: () => void; onCheck: () => void;
}) {
  const seconds = notice.grace_period_seconds;
  const grace = seconds % 60 === 0 ? `${seconds / 60}-minute` : `${seconds}-second`;
  return <section className="panel ended-game">
    <span className="eyebrow">RECONNECT GRACE EXPIRED</span>
    <h1>Your reconnect window expired.</h1>
    <h2 className="existing-town-name">{notice.town_name}</h2>
    <p role="status">You were away longer than the {grace} reconnect grace period. Your previous game has ended and cannot be continued.</p>
    {notice.former_town.is_active ? <>
      <p>Your former town is still active. Its current role vacancies are shown below.</p>
      {/* Vacancy metadata is ready for future joining; it must not activate an unimplemented join action. */}
      <div className="expired-role-grid" role="group" aria-label="Former town role availability">
        {notice.former_town.roles.map(slot => <button key={slot.role} className="secondary" disabled
          title={slot.vacancies === 0 ? 'This role is full.' : 'Joining an existing town is not implemented yet.'}>
          <strong>{slot.role === 'mayor' ? 'Mayor' : 'Commando'}</strong>
          <span>{slot.vacancies} open / {slot.capacity} capacity</span>
          <small>{slot.vacancies === 0 ? 'Role full' : 'Rejoining coming later'}</small>
        </button>)}
      </div>
      <p className="field-help">Rejoining existing towns will become available with town membership. Rejoining after expiry will start a fresh character at the townhall, with no saved cash or inventory restored.</p>
    </> : <p>Your former town is no longer active, so it cannot be selected. Start a new game and you will be assigned a random available townhall.</p>}
    {error && <div className="error" role="alert">{error}</div>}
    <div className="setup-actions">
      <button className="secondary" disabled={busy} onClick={onCheck}>Check again</button>
      <button className="primary" disabled={busy} onClick={onContinue}>{busy ? 'Continuing…' : 'Start a new game'} <span>↗</span></button>
    </div>
    <p className="field-help">This notice is dismissed across logins after you continue.</p>
  </section>;
}

function Game({ game, fallbackTownName, onLeave, onSignOut }: {
  game: ReturnType<typeof useGameConnection>; fallbackTownName: string;
  onLeave: () => void; onSignOut: () => void;
}) {
  const { user } = useAuth0();
  const hp = Math.max(0, Math.min(100, game.self.hp));
  const dead = game.self.ready && game.self.hp <= 0;
  const townName = game.snapshot.current?.towns?.find(t => t.mayor_player_id === game.playerId)?.name || fallbackTownName;
  return <div className="game-shell">
    <header className="game-header"><Brand /><div className="session">
      <span className="connection connected" role="status"><span className="dot" />Connected {game.ping !== null && <small>{game.ping} ms</small>}</span>
      <span className="username">{user?.name || user?.email || 'Mayor'}</span>
        <button className="secondary" disabled={game.quitting} onClick={onLeave}>Leave game</button>
        <button className="secondary" disabled={game.quitting} onClick={onSignOut}>Sign out</button>
    </div></header>
    <main className="game-main">
      <div className="world-toolbar"><span className="eyebrow">{townName} <span className="muted">/ MAYOR</span></span><span className="badge">ALPHA</span></div>
      {game.error && <div className="error" role="alert">{game.error}</div>}
      <div className="viewport">
        <GameCanvas snapshot={game.snapshot} input={game.input} />
        {dead && <div className="death-overlay" role="status"><h2>Down, but not out.</h2><p>Respawning at your townhall…</p></div>}
        <div className="location-chip">SECTOR {Math.floor(game.self.x / 3072)} : {Math.floor(game.self.y / 3072)}<small>{Math.round(game.self.x)}, {Math.round(game.self.y)}</small></div>
      </div>
      <div className="game-bottom">
        <section className="vitals"><span className="eyebrow">VITALS</span><div className="health"><span className="health-track"><span style={{ width: `${hp}%` }} /></span><b>{game.self.ready ? hp : '—'}<small> / 100</small></b></div></section>
        <section className="controls"><span><kbd>W</kbd><kbd>S</kbd> Move</span><span><kbd>A</kbd><kbd>D</kbd> Turn</span><span><kbd>SPACE</kbd> / LMB Fire</span></section>
        <span className="focus-hint">Click the world to take control</span>
      </div>
      <div className="world-footer"><span>Mayor of {townName} · 15-minute reconnect grace</span><span title={game.playerId}>SESSION {game.playerId.slice(0, 8).toUpperCase()}</span></div>
    </main>
  </div>;
}
