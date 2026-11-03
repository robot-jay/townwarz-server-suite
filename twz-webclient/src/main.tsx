import React from 'react';
import ReactDOM from 'react-dom/client';
import { Auth0Provider } from '@auth0/auth0-react';
import { App, ConfigurationScreen } from './App';
import { authConfigured, config } from './config';
import './styles.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {authConfigured ? (
      <Auth0Provider
        domain={config.auth0Domain}
        clientId={config.auth0ClientId}
        authorizationParams={{ redirect_uri: config.redirectUri }}
        onRedirectCallback={() => window.history.replaceState({}, document.title, window.location.pathname)}
      >
        <App />
      </Auth0Provider>
    ) : <ConfigurationScreen />}
  </React.StrictMode>,
);
