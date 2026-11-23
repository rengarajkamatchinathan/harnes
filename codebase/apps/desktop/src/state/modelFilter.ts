// Which listed models can chat. Providers list special-purpose models that advertise chat
// support but aren't chat models (computer use, deep research, agents, speech, live audio,
// image/video generation…); hide those by name so they can't be picked for a chat.
// Note: a provider error like `invalid_request` is NOT used to hide models — it can come from
// the request itself (e.g. a provider integration issue), not from the model.

/** Model ids/names of special-purpose families that can't answer a plain chat turn. */
const NOT_CHAT = [
  /computer[-_ ]?use/i,
  /deep[-_ ]?research/i,
  /antigravity/i,
  /\bagent\b/i,
  /[-_ ]tts\b|text[-_ ]to[-_ ]speech/i,
  /native[-_ ]audio|\blive\b|realtime|-audio\b/i,
  /image[-_ ](generation|preview)|\bimagen\b|\bveo\b/i,
  /robotics/i,
  /embed(ding)?/i,
  /\baqa\b/i,
];

export function isChatModel(m: { id: string; name?: string }): boolean {
  const s = `${m.id} ${m.name ?? ""}`;
  return !NOT_CHAT.some((re) => re.test(s));
}

/** Models to offer: chat-capable and from a ready provider. */
export function chatModels<T extends { id: string; name?: string; provider: string }>(models: T[], readyProviders: Set<string>): T[] {
  return models.filter((m) => readyProviders.has(m.provider) && isChatModel(m));
}

/** Thinking-effort levels that mean "no thinking"; not offered in the effort pickers. */
export const isOffEffort = (level: string) => /^(off|none|disabled|minimal-off)$/i.test(level.trim());
