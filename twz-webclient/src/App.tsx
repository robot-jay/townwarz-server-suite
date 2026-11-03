import { useState } from 'react';
import { useAuth0 } from '@auth0/auth0-react';
import { Shell } from './Layout';
import { NewGame } from './NewGame';

export function ConfigurationScreen() {
  return <Shell><main className="setup panel"><span className="eyebrow">CLIENT SETUP</span><h1>Your world awaits.</h1><p>Configure Auth0 to open the gates.</p><ol><li>Copy <code>.env.example</code> to <code>.env</code>.</li><li>Set <code>VITE_AUTH0_DOMAIN</code> and <code>VITE_AUTH0_CLIENT_ID</code>.</li><li>Add this origin to your Auth0 callback, logout, and web origin URLs: <code>{window.location.origin}</code>.</li><li>Restart the Vite development server.</li></ol><p className="muted">Use an Auth0 Single Page Application. See README.md for the full setup.</p></main></Shell>;
}

export function App() {
  const { isLoading, isAuthenticated, error, loginWithRedirect, logout } = useAuth0();
  const [loginError, setLoginError] = useState('');
  const login = () => { setLoginError(''); void loginWithRedirect().catch((e: unknown) => setLoginError(e instanceof Error ? e.message : 'Unable to start sign in. Please try again.')); };
  if (isLoading) return <Shell><main className="loading" role="status"><span className="spinner" /><p>Opening the gates…</p></main></Shell>;
  if (isAuthenticated && !error) return <NewGame />;
  return <Shell><main className="hero"><section className="hero-copy"><span className="eyebrow"><span className="dot" /> THE FRONTIER IS CALLING</span><h1>Every town<br />starts with <em>you.</em></h1><p>A shared world. A place to call home. Head into the frontier and make your mark in TownWarz.</p>{(error || loginError) && <div className="error" role="alert">{error?.message || loginError}</div>}<button className="primary" onClick={login}>Sign in & enter world <span>↗</span></button><p className="fine">Secure sign in with Auth0</p><div className="hero-notes"><span>01 / EXPLORE THE WORLD</span><span>02 / HOLD YOUR GROUND</span></div>{error && <button className="text-button" onClick={() => void logout({ logoutParams: { returnTo: window.location.origin } })}>Clear session & try again</button>}</section><section className="world-art" aria-hidden="true"><div className="art-grid" /><div className="art-water" /><div className="art-clearing" /><div className="art-town"><div className="roof" /><div className="hall"><span /><span /><span /></div><div className="door" /></div><div className="art-player">➤</div><div className="map-label">THE BEGINNING OF SOMETHING BIGGER<small>YOUR FIRST FOOTSTEP IS JUST AHEAD.</small></div><span className="map-coord">SECTOR 00 : 00</span></section></main></Shell>;
}
