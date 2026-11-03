const gatewayUrl = import.meta.env.VITE_GAME_WS_URL || `${window.location.protocol === 'https:' ? 'wss:' : 'ws:'}//${window.location.hostname}:8081/play`;

export const config = {
  auth0Domain: (import.meta.env.VITE_AUTH0_DOMAIN ?? '').trim(),
  auth0ClientId: (import.meta.env.VITE_AUTH0_CLIENT_ID ?? '').trim(),
  redirectUri: import.meta.env.VITE_AUTH0_REDIRECT_URI || window.location.origin,
  // Accept the previous .env endpoint while using the new authenticated entry.
  gameUrl: gatewayUrl.replace(/\/ws\/?$/, '/play'),
};

export const authConfigured = Boolean(config.auth0Domain && config.auth0ClientId);
