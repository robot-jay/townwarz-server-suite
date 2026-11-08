import { isConstructionCommand, rejectionMessage, type ConstructionCommand, type ConstructionIntent, type ConstructionReply, type ConstructionResult } from './construction';

export interface PendingConstruction { gameId: string; command: ConstructionCommand; phase: 'sending' | 'uncertain' }
export interface ConstructionNotice { kind: 'success' | 'error' | 'uncertain'; text: string; result?: ConstructionResult }
export interface ConstructionCommandsState { pending: PendingConstruction | null; notice: ConstructionNotice | null }
type StorageLike = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
const STORAGE_KEY = 'townwarz.pending-construction.v1';
function browserStorage(): StorageLike | undefined { try { return typeof window === 'undefined' ? undefined : window.sessionStorage; } catch { return undefined; } }
function copyCommand(command: ConstructionCommand): ConstructionCommand {
  return command.type === 'place_building'
    ? { type: command.type, request_id: command.request_id, building_type: command.building_type, origin_tile: { ...command.origin_tile } }
    : { type: command.type, request_id: command.request_id, building_id: command.building_id };
}

/** One unresolved intention per participation. Timeout/reconnect/reload replays
 * the exact request; neither a result's historical cash nor a preview mutates money. */
export class ConstructionCommands {
  state: ConstructionCommandsState = { pending: null, notice: null };
  private gameId: string | null = null;
  private send: ((command: ConstructionCommand) => boolean) | null = null;
  private timer: ReturnType<typeof setTimeout> | undefined;
  constructor(private changed: (state: ConstructionCommandsState) => void,
    private storage = browserStorage(), private timeoutMs = 10000) {}
  private emit() { this.changed({ ...this.state }); }
  private stopTimer() { clearTimeout(this.timer); this.timer = undefined; }
  private persist() {
    try {
      if (this.state.pending) this.storage?.setItem(STORAGE_KEY, JSON.stringify(this.state.pending));
      else this.storage?.removeItem(STORAGE_KEY);
    } catch { /* The live request still works if browser storage is unavailable. */ }
  }
  setSession(gameId: string, send: (command: ConstructionCommand) => boolean) {
    const changed = this.gameId !== gameId;
    this.send = send;
    if (!changed) return;
    this.stopTimer(); this.gameId = gameId;
    let pending: PendingConstruction | null = null;
    try {
      const value = JSON.parse(this.storage?.getItem(STORAGE_KEY) ?? 'null');
      if (value?.gameId === gameId && isConstructionCommand(value.command)) pending = { gameId, command: copyCommand(value.command), phase: 'uncertain' };
    } catch { /* Ignore invalid stored intentions. */ }
    this.state = { pending, notice: pending ? { kind: 'uncertain', text: 'An earlier action may have completed. Retry its original request to check safely.' } : null };
    this.persist(); this.emit();
  }
  submit(intent: ConstructionIntent): boolean {
    if (!this.gameId || !this.send || this.state.pending) return false;
    const command = copyCommand({ ...intent, request_id: crypto.randomUUID() });
    this.state = { pending: { gameId: this.gameId, command, phase: 'sending' }, notice: null };
    this.persist(); return this.transmit();
  }
  retry(): boolean {
    if (!this.send || !this.state.pending || this.state.pending.gameId !== this.gameId || this.state.pending.phase === 'sending') return false;
    return this.transmit();
  }
  private transmit(): boolean {
    const pending = this.state.pending;
    if (!pending) return false;
    this.stopTimer();
    this.state = { pending: { ...pending, phase: 'sending' }, notice: null };
    let sent = false;
    try { sent = this.send?.(copyCommand(pending.command)) ?? false; } catch { /* Retain the original intention. */ }
    if (sent) this.timer = setTimeout(() => this.uncertain('Confirmation timed out. This action may have completed; retry safely with the same request.'), this.timeoutMs);
    else this.uncertain('Connection unavailable. Your original request is saved for a safe retry.');
    this.persist(); this.emit(); return sent;
  }
  receive(reply: ConstructionReply): boolean {
    const pending = this.state.pending;
    if (!pending || pending.gameId !== this.gameId || pending.command.request_id !== reply.result.request_id) return false;
    this.stopTimer();
    if (!reply.outcome_known) {
      this.uncertain('The server could not confirm this action. It may have completed; retry the same request to check safely.');
    } else {
      const result = reply.result;
      this.state = { pending: null, notice: { kind: result.error ? 'error' : 'success',
        text: result.error ? rejectionMessage(result.error) : pending.command.type === 'place_building' ? 'Building placed.' : `Building demolished. ${result.refund.toLocaleString('en-US')} cash refunded.`, result } };
      this.persist(); this.emit();
    }
    return true;
  }
  private uncertain(text: string) {
    this.stopTimer();
    if (!this.state.pending) return;
    this.state = { pending: { ...this.state.pending, phase: 'uncertain' }, notice: { kind: 'uncertain', text } };
    this.persist(); this.emit();
  }
  disconnect() {
    this.send = null; this.stopTimer();
    if (this.state.pending) this.uncertain('Connection lost before confirmation. Reconnect and retry the saved request safely.');
  }
  endSession() {
    this.stopTimer(); this.gameId = null; this.send = null;
    this.state = { pending: null, notice: null }; this.persist(); this.emit();
  }
  dismissNotice() { if (!this.state.pending) { this.state = { ...this.state, notice: null }; this.emit(); } }
  dispose() { this.stopTimer(); this.send = null; }
}
