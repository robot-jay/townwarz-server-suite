import type { ReactNode } from 'react';

export function Brand() {
  return <a className="brand" href="/" aria-label="TownWarz home"><span className="brand-mark">T<span>W</span></span><span>TOWN<span className="gold">WARZ</span><small>A WORLD WORTH FIGHTING FOR</small></span></a>;
}

export function Shell({ children }: { children: ReactNode }) {
  return <div className="landing"><header><Brand /><span className="badge">EARLY ACCESS · ALPHA</span></header>{children}<footer>BUILD YOUR ROOTS. HOLD YOUR GROUND.<span>TownWarz · Development build</span></footer></div>;
}
