export type RecentRoom = { room: string; people: number; at: number };

export type Friend = { name: string; room: string; at: number; added?: boolean };

export type Mail = { id: string; author: string; text: string; at: number };

export type Thread = { name: string; room: string; notes: Mail[] };

export type Desk = {
  recents: RecentRoom[];
  friends: Friend[];
  threads: Thread[];
};

const KEY = "mycord.desk";
const EMPTY: Desk = { recents: [], friends: [], threads: [] };

export function loadDesk(): Desk {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return EMPTY;
    const parsed = JSON.parse(raw) as Partial<Desk>;
    return {
      recents: Array.isArray(parsed.recents) ? parsed.recents : [],
      friends: Array.isArray(parsed.friends) ? parsed.friends.flatMap(readFriend) : [],
      threads: Array.isArray(parsed.threads) ? parsed.threads : []
    };
  } catch {
    return EMPTY;
  }
}

export function saveDesk(desk: Desk) {
  localStorage.setItem(KEY, JSON.stringify(desk));
}

function readFriend(value: Partial<Friend> | null): Friend[] {
  if (!value || typeof value.name !== "string" || !value.name.trim()) return [];
  const friend: Friend = {
    name: value.name,
    room: typeof value.room === "string" ? value.room : "",
    at: typeof value.at === "number" ? value.at : 0
  };
  if (value.added === true) friend.added = true;
  return [friend];
}

export function ago(at: number) {
  const seconds = Math.max(0, (Date.now() - at) / 1000);
  if (seconds < 45) return "Just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  if (seconds < 172800) return "Yesterday";
  return `${Math.floor(seconds / 86400)}d ago`;
}

export function faceColor(name: string) {
  const faces = ["#e7d3b0", "#d5ddd4", "#e4cfc4", "#d5dbe4", "#ddd4c4", "#e8d4c6", "#d9d4c8"];
  let hash = 0;
  for (const char of name) hash = (hash * 33 + char.charCodeAt(0)) >>> 0;
  return faces[hash % faces.length];
}

export function initials(name: string) {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return "?";
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
  return `${parts[0][0]}${parts[1][0]}`.toUpperCase();
}
