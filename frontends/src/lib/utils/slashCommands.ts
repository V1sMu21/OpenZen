import { tSync } from "../i18n";

/** The slash-command catalog, shared by the palette (rendering) and the
 *  input (keyboard navigation) so ArrowUp/Down/Enter work without moving
 *  focus out of the textarea. */
export interface SlashCommand {
  command: string;
  description: string;
}

const COMMAND_KEYS: Array<[string, string]> = [
  ["/help", "cmd.help.desc"],
  ["/clear", "cmd.clear.desc"],
  ["/new", "cmd.new.desc"],
  ["/model", "cmd.model.desc"],
  ["/sessions", "cmd.sessions.desc"],
  ["/export", "cmd.export.desc"],
  ["/compact", "cmd.compact.desc"],
  ["/resume", "cmd.resume.desc"],
];

export function listCommands(lang: string): SlashCommand[] {
  return COMMAND_KEYS.map(([command, key]) => ({
    command,
    description: tSync(lang, key),
  }));
}

export function filterCommands(lang: string, filter: string): SlashCommand[] {
  const all = listCommands(lang);
  const f = filter.trim().toLowerCase();
  return f ? all.filter((c) => c.command.includes(f)) : all;
}
