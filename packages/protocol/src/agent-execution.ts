import { z } from "zod";

/** A terminal is the execution surface, not the provider's conversation identity. */
export const TerminalAgentExecutionSchema = z.object({
  kind: z.literal("terminal"),
  terminalId: z.string(),
  providerSessionId: z.string().optional(),
  profileId: z.string().optional(),
  icon: z.string().optional(),
});

export type TerminalAgentExecution = z.infer<typeof TerminalAgentExecutionSchema>;
