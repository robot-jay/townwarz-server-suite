import { useEffect, useState } from 'react';
import { config } from '../config';
import { catalogEndpoint, isBuildingCatalog, type BuildingCatalog } from './construction';

export function useBuildingCatalog(version: number | undefined) {
  const [catalog, setCatalog] = useState<BuildingCatalog | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    let active = true;
    let timedOut = false;
    const deadline = setTimeout(() => { timedOut = true; controller.abort(); }, 8000);
    setLoading(true); setError(''); setCatalog(null);
    void (async () => {
      try {
        const response = await fetch(catalogEndpoint(config.gameUrl), { signal: controller.signal, cache: 'no-cache', credentials: 'omit' });
        if (!response.ok) throw new Error(`Catalog request failed (${response.status}).`);
        const value: unknown = await response.json();
        if (!isBuildingCatalog(value)) throw new Error('The gateway returned an unsupported building catalog.');
        if (version !== undefined && value.version !== version) throw new Error('The catalog and game server versions do not match.');
        if (active && !controller.signal.aborted) setCatalog(value);
      } catch (error) {
        if (active && (!controller.signal.aborted || timedOut)) setError(timedOut ? 'The catalog request timed out.' : error instanceof Error ? error.message : 'Unable to load building definitions.');
      } finally {
        clearTimeout(deadline);
        if (active && (!controller.signal.aborted || timedOut)) setLoading(false);
      }
    })();
    return () => { active = false; clearTimeout(deadline); controller.abort(); };
  }, [version, attempt]);
  return { catalog, error, loading, retry: () => setAttempt(value => value + 1) };
}
