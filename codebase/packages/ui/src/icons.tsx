// A small stroke icon set (24×24 grid), drawn for Genie. No third-party icon fonts.

const PATHS = {
  plus: "M12 5v14M5 12h14",
  x: "M6 6l12 12M18 6L6 18",
  search: "M11 4a7 7 0 1 0 0 14a7 7 0 1 0 0-14zM20 20l-3.5-3.5",
  settings:
    "M12 9a3 3 0 1 0 0 6a3 3 0 1 0 0-6zM19.4 13a7.6 7.6 0 0 0 0-2l2-1.5-2-3.4-2.3 1a7.4 7.4 0 0 0-1.7-1L15 3.6h-4l-.4 2.5a7.4 7.4 0 0 0-1.7 1l-2.3-1-2 3.4 2 1.5a7.6 7.6 0 0 0 0 2l-2 1.5 2 3.4 2.3-1a7.4 7.4 0 0 0 1.7 1l.4 2.5h4l.4-2.5a7.4 7.4 0 0 0 1.7-1l2.3 1 2-3.4z",
  chat: "M4 5h16v11H9l-5 4z",
  code: "M8 8l-4 4 4 4M16 8l4 4-4 4M14 5l-4 14",
  send: "M5 12h13M13 6l6 6-6 6",
  stop: "M7 7h10v10H7z",
  paperclip: "M20 11.5l-8 8a5 5 0 0 1-7-7l8.5-8.5a3.5 3.5 0 0 1 5 5L10 17.5a2 2 0 0 1-3-3l7.5-7.5",
  chevronDown: "M6 9l6 6 6-6",
  chevronRight: "M9 6l6 6-6 6",
  chevronLeft: "M15 6l-6 6 6 6",
  copy: "M9 9h11v11H9zM5 15H4V4h11v1",
  check: "M5 12l5 5 9-10",
  pin: "M9 4h6l-1 6 4 3H6l4-3zM12 13v7",
  trash: "M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3",
  edit: "M4 20h4L19 9l-4-4L4 16zM14 6l4 4",
  refresh: "M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7",
  lock: "M6 11h12v9H6zM8 11V8a4 4 0 0 1 8 0v3",
  globe: "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18zM3 12h18M12 3c3 3.5 3 14.5 0 18M12 3c-3 3.5-3 14.5 0 18",
  terminal: "M4 5h16v14H4zM7 9l3 3-3 3M12 15h5",
  folder: "M3 6h6l2 2h10v11H3z",
  file: "M6 3h8l4 4v14H6zM14 3v4h4",
  branch: "M6 4v12M6 16a2 2 0 1 0 0 4a2 2 0 1 0 0-4zM18 4a2 2 0 1 0 0 4a2 2 0 1 0 0-4zM18 8c0 5-8 4-12 8",
  more: "M5 12h.01M12 12h.01M19 12h.01",
  incognito: "M3 12h18M6 12l2-6h8l2 6M7 17a2 2 0 1 0 0-.01M17 17a2 2 0 1 0 0-.01M9 17h6",
  sparkle: "M12 3l2 6 6 2-6 2-2 6-2-6-6-2 6-2z",
  user: "M12 4a4 4 0 1 0 0 8a4 4 0 1 0 0-8zM4 20c1.5-4 14.5-4 16 0",
  panel: "M4 5h16v14H4zM15 5v14",
  download: "M12 4v11M7 10l5 5 5-5M5 20h14",
  external: "M14 4h6v6M20 4l-9 9M18 14v6H4V6h6",
  warning: "M12 4l9 16H3zM12 10v4M12 17h.01",
  info: "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18zM12 11v6M12 7h.01",
  play: "M8 5l11 7-11 7z",
  pause: "M8 5v14M16 5v14",
  hand: "M8 13V6a1.5 1.5 0 0 1 3 0v6M11 11V4.5a1.5 1.5 0 0 1 3 0V11M14 11V6a1.5 1.5 0 0 1 3 0v8a6 6 0 0 1-12 1l-1.5-3a1.5 1.5 0 0 1 2.6-1.5L8 13",
  maximize: "M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5",
  thought: "M9 18h6M10 21h4M12 3a6 6 0 0 0-3.5 10.9V16h7v-2.1A6 6 0 0 0 12 3z",
  tool: "M14.5 5.5a4 4 0 0 0 4.9 4.9l-9 9a2 2 0 0 1-3-3l9-9a4 4 0 0 0-1.9-1.9z",
  memory: "M5 4h14v16H5zM9 8h6M9 12h6M9 16h3",
  layers: "M12 3l9 5-9 5-9-5zM3 13l9 5 9-5",
} as const;

export type IconName = keyof typeof PATHS;
export const ICON_NAMES = Object.keys(PATHS) as IconName[];

export function Icon({ name, size = 16, title }: { name: IconName; size?: number; title?: string }) {
  return (
    <svg className="g-icon" width={size} height={size} viewBox="0 0 24 24" aria-hidden={title ? undefined : true} role={title ? "img" : undefined}>
      {title && <title>{title}</title>}
      <path d={PATHS[name]} />
    </svg>
  );
}
