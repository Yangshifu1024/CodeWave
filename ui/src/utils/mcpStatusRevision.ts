// Local event clock: an IPC snapshot started before an accepted status event may be stale
// even when both have the same connection generation (Starting -> Ready).
let revision = 0;
export const mcpStatusEventRevision = () => revision;
export function markMcpStatusEvent(): void { revision += 1; }
