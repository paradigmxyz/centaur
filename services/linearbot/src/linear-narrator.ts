import type { Logger, Thread } from "chat";
import type { LinearSessionCapableAdapter } from "./types";
import { errorMessage } from "./utils";

/**
 * Best-effort working acknowledgement: an ephemeral thought via the adapter's
 * startTyping. Fired when a (vestigial) agent session starts — Linear expects a
 * thought within 10 seconds — before the session is settled. Never throws.
 */
export function ackWorking(thread: Thread, logger: Logger): void {
  const adapter = thread.adapter as unknown as LinearSessionCapableAdapter;
  if (!adapter.startTyping) return;
  void adapter.startTyping(thread.id, "Looking into it…").catch((error) => {
    logger.debug("linearbot_ack_failed", { error: errorMessage(error) });
  });
}
