import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import ramenUrl from "./ramen.svg";
import { faceColor, initials, loadDesk, saveDesk, type Desk, type Mail } from "./desk";
import {
  ChatBubbleIcon,
  ChevronIcon,
  ChevronRightIcon,
  FullscreenExitIcon,
  FullscreenIcon,
  HeadphonesIcon,
  HeadphonesOffIcon,
  MicIcon,
  MicOffIcon,
  MonitorIcon,
  PersonAddIcon,
  PersonSearchIcon,
  PhoneIcon,
  RamenMark,
  SendIcon,
  SettingsIcon,
  ShareIcon,
  StopShareIcon,
  SmileIcon,
  VideoIcon,
  VisibilityIcon,
  VisibilityOffIcon
} from "./icons";
import { playEnter, playExit, playJoined, playLeft, playMute, playUnmute, primeAudio } from "./sounds";

type Note =
  | { kind: "status"; text: string }
  | { kind: "joined" }
  | { kind: "left" }
  | { kind: "sharing"; on: boolean }
  | { kind: "watching"; on: boolean }
  | { kind: "people"; people: Person[] };

type Person = { name: string; local: boolean; speaking: boolean; muted?: boolean };

type Menu = "share" | "audio" | "hear" | "devices" | "stream" | "resolution" | "fps" | "bitrate" | "screens" | null;
type StageView = "screen" | "remote" | "self";
type TileCorner = "tl" | "tr" | "bl" | "br";
const RESOLUTIONS = [
  { width: 1280, height: 720, label: "1280 × 720" },
  { width: 1920, height: 1080, label: "1920 × 1080" },
  { width: 2560, height: 1440, label: "2560 × 1440" }
];

const BITRATES = [8000, 12000, 20000, 40000, 80000, 100000];

function pairRoom(mine: string, theirs: string) {
  const names = [mine, theirs]
    .map((item) => item.trim().toLowerCase().replace(/[^a-z0-9_-]+/g, ""))
    .filter(Boolean)
    .sort();
  return names.length ? `yatai-${names.join("-")}`.slice(0, 64) : "";
}

type ScreenChoice = { id: number; label: string; width: number; height: number };

type Profile = {
  name: string;
  room: string;
  invite: string;
  resolution: number;
  fps: number;
  bitrateKbps: number;
  dark: boolean;
  voice: number;
  screen: number;
  input: string;
  output: string;
  noise: boolean;
};

type Gear = { id: string; name: string };

export function App() {
  const [theme, setTheme] = useState<"light" | "dark">("light");
  const [name, setName] = useState("");
  const [room, setRoom] = useState("");
  const [invite, setInvite] = useState("");
  const [phase, setPhase] = useState<"home" | "joining" | "call">("home");
  const [status, setStatus] = useState("");
  const [sharing, setSharing] = useState(false);
  const [watching, setWatching] = useState(false);
  const [muted, setMuted] = useState(false);
  const [deaf, setDeaf] = useState(false);
  const [resolution, setResolution] = useState(1);
  const [fps, setFps] = useState(60);
  const [bitrate, setBitrate] = useState(40000);
  const [voice, setVoice] = useState(100);
  const [screen, setScreen] = useState(100);
  const [inputId, setInputId] = useState("");
  const [outputId, setOutputId] = useState("");
  const [noise, setNoise] = useState(true);
  const [inputs, setInputs] = useState<Gear[]>([]);
  const [outputs, setOutputs] = useState<Gear[]>([]);
  const [fullscreen, setFullscreen] = useState(false);
  const [chrome, setChrome] = useState(true);
  const [displayId, setDisplayId] = useState<number | null>(null);
  const [picker, setPicker] = useState<ScreenChoice[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [people, setPeople] = useState<Person[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [desk, setDesk] = useState<Desk>(loadDesk);
  const [menu, setMenu] = useState<Menu>(null);
  const [devicePane, setDevicePane] = useState<"input" | "output" | null>(null);
  const [popAt, setPopAt] = useState<{ x: number; y: number } | null>(null);
  const [slots, setSlots] = useState<{ main: StageView; a: StageView; b: StageView }>({ main: "screen", a: "remote", b: "self" });
  const [tileAt, setTileAt] = useState<{ x: number; y: number } | null>(null);
  const [tileDragging, setTileDragging] = useState(false);
  const [tileCorner, setTileCorner] = useState<TileCorner>("br");
  const [paneShift, setPaneShift] = useState(0);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [prefsOpen, setPrefsOpen] = useState(false);
  const [session, setSession] = useState(() => {
    const preview = import.meta.env.DEV ? new URLSearchParams(window.location.search).get("preview") : null;
    if (preview === "login" || preview === "register") return false;
    if (preview) return true;
    const saved = localStorage.getItem("mycord.session");
    return Boolean(saved && saved !== "1");
  });
  const [authMode, setAuthMode] = useState<"login" | "register">(() => (
    import.meta.env.DEV && new URLSearchParams(window.location.search).get("preview") === "register" ? "register" : "login"
  ));
  const [showPassword, setShowPassword] = useState(false);
  const [authEmail, setAuthEmail] = useState("");
  const [authUser, setAuthUser] = useState("");
  const [authPass, setAuthPass] = useState("");
  const [dobDay, setDobDay] = useState("");
  const [dobMonth, setDobMonth] = useState("");
  const [dobYear, setDobYear] = useState("");
  const [authNote, setAuthNote] = useState("");
  const [authBusy, setAuthBusy] = useState(false);
  const [friendOpen, setFriendOpen] = useState(false);
  const [friendQuery, setFriendQuery] = useState("");
  const [foundPeople, setFoundPeople] = useState<string[]>([]);
  const sharingRef = useRef(false);
  const remotesRef = useRef<Set<string> | null>(null);
  const noticeTimer = useRef(0);
  const roomRef = useRef(room);
  const pendingShare = useRef(false);
  const heard = useRef({ voice: 100, screen: 100 });
  const shareRef = useRef<() => Promise<void>>(async () => { });
  const panelRef = useRef<HTMLDivElement>(null);
  const micMenuRef = useRef<HTMLDivElement>(null);
  const tileDrag = useRef<{ dx: number; dy: number; sx: number; sy: number } | null>(null);
  const tileCornerRef = useRef<TileCorner>("br");
  const tileAtRef = useRef<{ x: number; y: number } | null>(null);
  const postStageRef = useRef<() => void>(() => { });
  roomRef.current = room;
  tileCornerRef.current = tileCorner;
  tileAtRef.current = tileAt;

  const saved = (next?: Partial<Profile>): Profile => ({
    name,
    room,
    invite,
    resolution,
    fps,
    bitrateKbps: bitrate,
    dark: theme === "dark",
    voice,
    screen,
    input: inputId,
    output: outputId,
    noise,
    ...next
  });

  function persist(next?: Partial<Profile>) {
    void invoke("remember", { body: saved(next) }).catch(() => { });
  }

  function loadDevices() {
    void invoke<Gear[]>("inputs").then(setInputs).catch(() => { });
    void invoke<Gear[]>("outputs").then(setOutputs).catch(() => { });
  }

  function pickInput(id: string) {
    setInputId(id);
    persist({ input: id });
    void invoke("set_input", { body: { id } }).catch(() => { });
  }

  function pickOutput(id: string) {
    setOutputId(id);
    persist({ output: id });
    void invoke("set_output", { body: { id } }).catch(() => { });
  }

  function pickNoise(on: boolean) {
    setNoise(on);
    persist({ noise: on });
    void invoke("set_noise", { body: { on } }).catch(() => { });
  }

  function commit(recipe: (current: Desk) => Desk) {
    setDesk((current) => {
      const next = recipe(current);
      saveDesk(next);
      return next;
    });
  }

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (event: MediaQueryListEvent) => {
      const next = event.matches ? "dark" : "light";
      setTheme(next);
      persist({ dark: next === "dark" });
    };
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);

  useEffect(() => {
    loadDevices();
  }, [phase]);

  useEffect(() => {
    if (!import.meta.env.DEV) return;
    const preview = new URLSearchParams(window.location.search).get("preview");
    if (!preview) return;
    const now = Date.now();
    setTheme("dark");
    setName("Avyukt");
    setRoom("project-sync");
    setDesk({
      recents: [
        { room: "mycord-room", people: 2, at: now - 12 * 60_000 },
        { room: "project-sync", people: 2, at: now - 3_600_000 },
        { room: "game-night", people: 3, at: now - 86_400_000 }
      ],
      friends: [
        { name: "souvlakee", room: "project-sync", at: now - 120_000 },
        { name: "Asna Fatimah", room: "project-sync", at: now - 3_600_000 },
        { name: "malubulu", room: "mycord-room", at: now - 7_200_000 },
        { name: "Divyank Shandilya", room: "project-sync", at: now - 86_400_000 },
        { name: "beat_n", room: "game-night", at: now - 172_800_000 }
      ],
      threads: [
        {
          name: "souvlakee",
          room: "project-sync",
          notes: [
            { id: "1", author: "Avyukt", text: "hey, free for a quick session?", at: now - 26 * 60_000 },
            { id: "2", author: "souvlakee", text: "yeah, in a bit", at: now - 22 * 60_000 },
            { id: "3", author: "souvlakee", text: "wanna hop on a call and go through the project?", at: now - 20 * 60_000 },
            { id: "4", author: "Avyukt", text: "sure, let's do it", at: now - 18 * 60_000 }
          ]
        }
      ]
    });
    if (preview === "login" || preview === "register") {
      setSession(false);
      setAuthMode(preview);
      return;
    }
    setSession(true);
    if (preview === "inbox" || preview === "add" || preview === "search") {
      setActive("");
      if (preview === "add" || preview === "search") setFriendOpen(true);
      if (preview === "search") setFriendQuery("asna");
      return;
    }
    if (preview !== "inbox") setActive("souvlakee");
    if (preview === "call" || preview === "share" || preview === "watch" || preview === "options" || preview === "mics" || preview === "screens") {
      setPhase("call");
      setSharing(preview === "share" || preview === "options");
      if (preview === "options") {
        setMenu("resolution");
        setPicker([
          { id: 1, label: "LG ULTRAGEAR", width: 1920, height: 1080 },
          { id: 2, label: "LG FHD", width: 1080, height: 1920 }
        ]);
      }
      if (preview === "mics") setMenu("devices");
      if (preview === "screens") {
        setMenu("screens");
        setPicker([
          { id: 1, label: "LG ULTRAGEAR", width: 1920, height: 1080 },
          { id: 2, label: "LG FHD", width: 1080, height: 1920 }
        ]);
      }
      setPeople([
        { name: "Avyukt", local: true, speaking: false },
        { name: "souvlakee", local: false, speaking: true }
      ]);
      setWatching(preview === "watch");
      setStatus("In the room");
    } else if (preview === "inbox") {
      setActive("");
    }
  }, []);

  useEffect(() => {
    void invoke<Profile>("profile")
      .then((stored) => {
        if (new URLSearchParams(window.location.search).has("preview") && import.meta.env.DEV) return;
        const accountName = localStorage.getItem("mycord.user");
        setName(accountName || stored.name);
        setRoom(stored.room);
        setInvite(stored.invite);
        setResolution(stored.resolution);
        setFps(stored.fps);
        setBitrate(stored.bitrateKbps);
        setTheme(stored.dark ? "dark" : "light");
        setVoice(stored.voice ?? 100);
        setScreen(stored.screen ?? 100);
        setInputId(stored.input ?? "");
        setOutputId(stored.output ?? "");
        const suppression = stored.noise ?? true;
        setNoise(suppression);
        void invoke("set_noise", { body: { on: suppression } }).catch(() => { });
      })
      .catch(() => { });
  }, []);

  function flash(text: string) {
    setNotice(text);
    window.clearTimeout(noticeTimer.current);
    noticeTimer.current = window.setTimeout(() => setNotice(null), 2400);
  }

  useEffect(() => {
    const unlisten = listen<Note>("engine", (event) => {
      const note = event.payload;
      if (note.kind === "status") {
        setStatus(note.text);
        setPhase((current) => (current === "joining" ? "home" : current));
        setBusy(false);
      } else if (note.kind === "joined") {
        setPhase("call");
        setStatus("");
        setBusy(false);
        remotesRef.current = null;
        playJoined();
        const roomName = roomRef.current;
        commit((current) => ({
          ...current,
          recents: [{ room: roomName, people: 1, at: Date.now() }, ...current.recents.filter((item) => item.room !== roomName)].slice(0, 20)
        }));
      } else if (note.kind === "left") {
        setPhase("home");
        setPeople([]);
        setSharing(false);
        setWatching(false);
        sharingRef.current = false;
        remotesRef.current = null;
        setMuted(false);
        setDeaf(false);
        setFullscreen(false);
        setNotice(null);
        setMenu(null);
        setStatus("");
        setBusy(false);
        playLeft();
        void invoke("set_fullscreen", { body: { on: false } });
      } else if (note.kind === "sharing") {
        setSharing(note.on);
        sharingRef.current = note.on;
        setStatus(note.on ? "Sharing your screen." : "");
      } else if (note.kind === "watching") {
        setWatching(note.on);
      } else if (note.kind === "people") {
        setPeople(note.people);
        const remotes = note.people.filter((person) => !person.local);
        const names = remotes.map((person) => person.name);
        const previous = remotesRef.current;
        remotesRef.current = new Set(names);
        if (previous) {
          for (const person of names) {
            if (!previous.has(person)) {
              flash(`${person} joined`);
              playEnter();
            }
          }
          for (const person of previous) {
            if (!names.includes(person)) {
              flash(`${person} left`);
              playExit();
            }
          }
        } else if (names.length > 0) {
          window.setTimeout(() => playEnter(), 220);
        }
        if (!sharingRef.current) {
          setStatus((current) => (
            /microphone|could not|did not/i.test(current) ? current : ""
          ));
        }
        if (remotes.length > 0) {
          commit((current) => ({
            ...current,
            recents: current.recents.map((item, index) => (index === 0 ? { ...item, people: note.people.length } : item))
          }));
        }
      }
    });
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, []);

  useEffect(() => {
    if (!fullscreen) {
      setChrome(true);
      return;
    }
    let timer = 0;
    const show = () => {
      setChrome(true);
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setChrome(false), 1800);
    };
    show();
    window.addEventListener("mousemove", show);
    return () => {
      window.removeEventListener("mousemove", show);
      window.clearTimeout(timer);
    };
  }, [fullscreen]);

  useEffect(() => {
    setSlots({ main: "screen", a: "remote", b: "self" });
    if (!tileDrag.current) setTileAt(null);
  }, [sharing, watching]);

  useEffect(() => {
    if (phase !== "call") return;
    const onResize = () => {
      const panel = panelRef.current;
      const stack = panel?.querySelector(".tile-stack") as HTMLElement | null;
      const origin = stack?.offsetParent;
      if (!panel || !stack || !(origin instanceof HTMLElement)) return;
      const box = origin.getBoundingClientRect();
      /* Always clamp tile-stack inside the visible panel area */
      if (tileAtRef.current && !tileDrag.current) {
        const corner = tileCornerRef.current;
        setTileAt({
          x: Math.min(box.width - stack.offsetWidth - 12, Math.max(12, corner.endsWith("l") ? 12 : box.width - stack.offsetWidth - 12)),
          y: Math.min(box.height - stack.offsetHeight - 12, Math.max(12, corner.startsWith("t") ? 12 : box.height - stack.offsetHeight - 12))
        });
      }
    };
    window.addEventListener("resize", onResize);
    /* Clamp once on mount as well */
    requestAnimationFrame(onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [phase]);

  useLayoutEffect(() => {
    if (menu !== "devices" || !devicePane || !micMenuRef.current) return;
    const row = micMenuRef.current.querySelector(`[data-pane="${devicePane}"]`);
    if (!row) return;
    const menuBox = micMenuRef.current.getBoundingClientRect();
    const rowBox = row.getBoundingClientRect();
    setPaneShift(rowBox.top - menuBox.top);
  }, [menu, devicePane, inputs, outputs]);

  postStageRef.current = () => {
    if (phase !== "call") return;
    const stage = document.getElementById("stage");
    const panel = panelRef.current;
    if (!stage || !panel) return;
    const stageRect = stage.getBoundingClientRect();
    const presenting = sharing || watching;
    const screenTile = document.getElementById("screen-tile");
    const shrunk = Boolean(presenting && screenTile && slots.main !== "screen");
    let x = stageRect.x;
    let y = stageRect.y;
    let width = stageRect.width;
    let height = stageRect.height;
    let radius = fullscreen ? 0 : 15;
    const holes: { x: number; y: number; width: number; height: number }[] = [];
    if (shrunk && screenTile) {
      const tile = screenTile.getBoundingClientRect();
      x = tile.x;
      y = tile.y;
      width = tile.width;
      height = tile.height;
      radius = 12;
    } else {
      const cut = (node: Element) => {
        if (fullscreen && Number(getComputedStyle(node).opacity) <= 0.02) return;
        const box = node.getBoundingClientRect();
        if (box.width < 2 || box.height < 2) return;
        const left = Math.round(box.x - x);
        const top = Math.round(box.y - y);
        const right = Math.round(box.right - x);
        const bottom = Math.round(box.bottom - y);
        holes.push({ x: left, y: top, width: Math.max(2, right - left), height: Math.max(2, bottom - top) });
      };
      panel.querySelectorAll(".face-tile, .name-pill, .controls, .hangup, .voice-pop, .mock-menu, .pop-anchor").forEach(cut);
    }
    void invoke("stage_rect", {
      body: { x, y, width: Math.max(2, width), height: Math.max(2, height), visible: true, radius, holes }
    }).catch(() => { });
  };

  useEffect(() => {
    if (phase !== "call") {
      void invoke("stage_rect", { body: { x: 0, y: 0, width: 0, height: 0, visible: false, radius: 0, holes: [] } }).catch(() => { });
      return;
    }
    const post = () => postStageRef.current();
    post();
    const observer = new ResizeObserver(post);
    const stage = document.getElementById("stage");
    if (stage) observer.observe(stage);
    if (panelRef.current) observer.observe(panelRef.current);
    window.addEventListener("resize", post);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", post);
      void invoke("stage_rect", { body: { x: 0, y: 0, width: 0, height: 0, visible: false, radius: 0, holes: [] } }).catch(() => { });
    };
  }, [phase]);

  useLayoutEffect(() => {
    if (phase !== "call") return;
    postStageRef.current();
    if (!fullscreen) return;
    let frame = 0;
    const start = performance.now();
    const tick = (now: number) => {
      postStageRef.current();
      if (now - start < 300) {
        frame = requestAnimationFrame(tick);
      }
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [phase, fullscreen, chrome, sharing, watching, slots, tileCorner, tileAt, menu]);

  useEffect(() => {
    if (phase !== "call") return;
    void invoke("set_voice", { body: { percent: voice } }).catch(() => { });
    void invoke("set_screen", { body: { percent: screen } }).catch(() => { });
  }, [phase, voice, screen]);

  useEffect(() => {
    if (!menu) return;
    const close = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Element && target.closest(".tool, .strip, .mock-menu, .slider-card, .voice-pop")) return;
      setMenu(null);
      setDevicePane(null);
    };
    window.addEventListener("pointerdown", close);
    return () => window.removeEventListener("pointerdown", close);
  }, [menu]);

  useEffect(() => {
    if (phase === "call" && pendingShare.current) {
      pendingShare.current = false;
      void shareRef.current();
    }
  }, [phase]);

  useEffect(() => {
    const syncFullscreen = () => {
      void getCurrentWindow().isFullscreen().then((isFull) => {
        setFullscreen(isFull);
      }).catch(() => { });
    };
    window.addEventListener("resize", syncFullscreen);
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        document.getElementById("thread-search")?.focus();
      }
      if (event.key === "Escape") {
        setPrefsOpen(false);
        setMenu(null);
        setDevicePane(null);
        setFullscreen((current) => {
          if (current) {
            void invoke("set_fullscreen", { body: { on: false } }).catch(() => { });
          }
          return false;
        });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("resize", syncFullscreen);
      window.removeEventListener("keydown", onKey);
    };
  }, []);

  async function run(task: () => Promise<unknown>, settle = true) {
    setBusy(true);
    try {
      await task();
      if (settle) setBusy(false);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
      setBusy(false);
      setPhase((current) => (current === "joining" ? "home" : current));
    }
  }

  const picture = () => {
    const size = RESOLUTIONS[resolution] ?? RESOLUTIONS[1];
    return { width: size.width, height: size.height, fps, bitrateKbps: bitrate, displayId };
  };

  async function loadScreens() {
    try {
      return await invoke<ScreenChoice[]>("screens");
    } catch {
      return [];
    }
  }

  async function shareScreen() {
    if (sharingRef.current) {
      await invoke("stop_share");
      return;
    }
    const found = await loadScreens();
    if (found.length > 1) {
      setPicker(found);
      setMenu("screens");
      return;
    }
    const next = found[0]?.id ?? null;
    setDisplayId(next);
    await invoke("share", { body: { ...picture(), displayId: next } });
  }

  async function showScreens() {
    setPicker(await loadScreens());
    setMenu("screens");
  }

  async function useScreen(id: number) {
    setPicker(null);
    setDisplayId(id);
    setMenu(null);
    if (sharingRef.current) await invoke("stop_share");
    await invoke("share", { body: { ...picture(), displayId: id } });
  }
  shareRef.current = shareScreen;

  function joinRoom(nextRoom: string, share = false) {
    const accountName = localStorage.getItem("mycord.user");
    const nextName = (accountName || name).trim();
    const target = nextRoom.trim();
    if (!nextName || !target) {
      setStatus("Choose a friend to call.");
      return;
    }
    primeAudio();
    pendingShare.current = share;
    setName(nextName);
    setRoom(target);
    setPrefsOpen(false);
    setPhase("joining");
    setStatus("Joining the room.");
    persist({ name: nextName, room: target });
    const saved = localStorage.getItem("mycord.session");
    void run(() => invoke("join", { body: { name: nextName, room: target, invite: invite.trim() ? invite.trim() : null, session: saved && saved !== "1" ? saved : null } }), false);
  }
  // Call buttons stay inactive until calling is turned on.
  void joinRoom;

  function rememberAccount(account: { username: string; session: string }) {
    localStorage.setItem("mycord.session", account.session);
    localStorage.setItem("mycord.user", account.username);
    enterApp(account.username);
    setAuthPass("");
  }

  function enterApp(displayName: string) {
    const next = displayName.trim();
    if (next) {
      setName(next);
      localStorage.setItem("mycord.user", next);
      persist({ name: next });
    }
    setSession(true);
    setAuthNote("");
  }

  useEffect(() => {
    if (import.meta.env.DEV && new URLSearchParams(window.location.search).has("preview")) return;
    const saved = localStorage.getItem("mycord.session");
    if (!saved || saved === "1") {
      if (saved === "1") localStorage.removeItem("mycord.session");
      setSession(false);
      return;
    }
    let cancel = false;
    void invoke<{ username: string; session: string }>("account", { body: { action: "session", session: saved } })
      .then((account) => {
        if (cancel) return;
        rememberAccount(account);
      })
      .catch((err: unknown) => {
        if (cancel) return;
        localStorage.removeItem("mycord.session");
        localStorage.removeItem("mycord.user");
        setAuthNote(accountError(err));
        setSession(false);
      });
    return () => {
      cancel = true;
    };
  }, []);

  useEffect(() => {
    if (!session) return;
    const ping = () => {
      const saved = localStorage.getItem("mycord.session");
      if (!saved || saved === "1") return;
      void invoke("account", { body: { action: "here", session: saved } }).catch(() => { });
    };
    ping();
    const timer = window.setInterval(ping, 20000);
    return () => window.clearInterval(timer);
  }, [session]);

  useEffect(() => {
    if (!friendOpen) {
      setFoundPeople([]);
      return;
    }
    const query = friendQuery.trim().replace(/^@/, "");
    if (query.length < 2) {
      setFoundPeople([]);
      return;
    }
    const saved = localStorage.getItem("mycord.session");
    if (!saved || saved === "1") return;
    let cancel = false;
    const timer = window.setTimeout(() => {
      void invoke<string[]>("people", { body: { session: saved, query } })
        .then((names) => {
          if (!cancel) setFoundPeople(names);
        })
        .catch(() => {
          if (!cancel) setFoundPeople([]);
        });
    }, 180);
    return () => {
      cancel = true;
      window.clearTimeout(timer);
    };
  }, [friendOpen, friendQuery]);

  function logOut() {
    const saved = localStorage.getItem("mycord.session");
    localStorage.removeItem("mycord.session");
    localStorage.removeItem("mycord.user");
    setAuthPass("");
    setAuthNote("");
    setPrefsOpen(false);
    setSession(false);
    if (phase === "call" || phase === "joining") void invoke("leave").catch(() => { });
    if (saved && saved !== "1") void invoke("account", { body: { action: "logout", session: saved } }).catch(() => { });
  }

  function addFoundFriend(foundName: string) {
    const nextName = foundName.replace(/^@/, "").trim();
    if (!nextName) return;
    const mine = (localStorage.getItem("mycord.user") || name).trim();
    const target = pairRoom(mine, nextName);
    commit((current) => ({
      ...current,
      friends: [{ name: nextName, room: target, at: Date.now(), added: true }, ...current.friends.filter((friend) => friend.name.toLowerCase() !== nextName.toLowerCase())].slice(0, 40)
    }));
    setFriendQuery("");
    setFriendOpen(false);
    openFriend({ name: nextName, room: target });
  }

  function openFriend(friend: { name: string; room: string }) {
    setActive(friend.name);
    commit((current) => {
      if (current.threads.some((item) => item.name === friend.name)) return current;
      return { ...current, threads: [{ name: friend.name, room: friend.room, notes: [] }, ...current.threads] };
    });
  }

  function sendNote(who: string, roomName: string) {
    const text = draft.trim();
    if (!text) return;
    const mail = { id: `${Date.now()}`, author: name.trim() || "You", text, at: Date.now() };
    commit((current) => {
      const existing = current.threads.find((thread) => thread.name === who);
      const threads = existing
        ? current.threads.map((thread) => (thread.name === who ? { ...thread, room: roomName || thread.room, notes: [...thread.notes, mail] } : thread))
        : [{ name: who, room: roomName, notes: [mail] }, ...current.threads];
      return { ...current, threads };
    });
    setDraft("");
  }

  function tune(next: Partial<{ resolution: number; fps: number; bitrate: number }>) {
    const resolutionNext = next.resolution ?? resolution;
    const fpsNext = next.fps ?? fps;
    const bitrateNext = next.bitrate ?? bitrate;
    if (next.resolution !== undefined) setResolution(resolutionNext);
    if (next.fps !== undefined) setFps(fpsNext);
    if (next.bitrate !== undefined) setBitrate(bitrateNext);
    persist({ resolution: resolutionNext, fps: fpsNext, bitrateKbps: bitrateNext });
    const size = RESOLUTIONS[resolutionNext] ?? RESOLUTIONS[1];
    void invoke("tune", { body: { width: size.width, height: size.height, fps: fpsNext, bitrateKbps: bitrateNext } }).catch(() => { });
  }

  function changeVoice(next: number) {
    setDeaf(false);
    setVoice(next);
    persist({ voice: next });
  }

  function changeScreen(next: number) {
    setDeaf(false);
    setScreen(next);
    persist({ screen: next });
  }

  function toggleDeaf() {
    primeAudio();
    if (deaf) {
      changeVoice(heard.current.voice || 100);
      changeScreen(heard.current.screen || 100);
      setDeaf(false);
      return;
    }
    heard.current = { voice, screen };
    setVoice(0);
    setScreen(0);
    persist({ voice: 0, screen: 0 });
    setDeaf(true);
  }

  const online = new Set(people.filter((person) => !person.local).map((person) => person.name));
  const rail = (
    <Rail
      prefsOpen={prefsOpen}
      onPrefs={() => setPrefsOpen((open) => !open)}
      onTheme={() => {
        const next = theme === "dark" ? "light" : "dark";
        setTheme(next);
        persist({ dark: next === "dark" });
      }}
      onLogOut={logOut}
      dark={theme === "dark"}
    />
  );

  if (phase === "call") {
    const others = people.filter((person) => !person.local);
    const featured = others.find((person) => person.speaking) ?? others[0];
    const you = people.find((person) => person.local);
    const talking = Boolean(featured && featured.speaking);
    const size = RESOLUTIONS[resolution] ?? RESOLUTIONS[1];
    const volumeOpen = menu === "audio" || menu === "hear";
    const shareOpen = menu === "share" || menu === "screens" || menu === "stream" || menu === "resolution" || menu === "fps" || menu === "bitrate";
    const inputName = inputs.find((item) => item.id === inputId)?.name || "System default";
    const outputName = outputs.find((item) => item.id === outputId)?.name || "System default";
    const placePop = (clientX: number, clientY: number) => {
      const panel = panelRef.current?.getBoundingClientRect();
      if (!panel) return;
      const popW = 196;
      const popH = 44;
      setPopAt({
        x: Math.max(8, Math.min(clientX - panel.left, panel.width - popW - 8)),
        y: Math.max(8, Math.min(clientY - panel.top, panel.height - popH - 8))
      });
    };
    const presenting = sharing || watching;
    const mainPerson = slots.main === "self" ? (you?.name || name || "You") : (featured?.name || you?.name || name || "You");
    const showCenter = !presenting || slots.main !== "screen";
    const pillOnScreen = presenting && slots.main === "screen";
    const pillPerson = pillOnScreen
      ? (sharing ? you : featured)
      : (slots.main === "self" || !featured ? you : featured);
    const pillName = pillPerson?.name || (presenting ? mainPerson : "Just you");
    const pillMuted = pillPerson ? (pillPerson.local ? muted : Boolean(pillPerson.muted)) : false;
    const renderTile = (view: StageView, slot: "a" | "b") => {
      if (view === "remote" && !featured) return null;
      const person = view === "self" ? (you?.name || name || "You") : (featured?.name || "Guest");
      const speaking = view === "self" ? Boolean(you?.speaking && !muted) : talking;
      return (
        <div
          key={slot}
          className={`face-tile${view === "self" ? " you-tile" : ""}${speaking ? " speaking" : ""}${view === "screen" ? " screen-tile" : ""}`}
          data-tile={slot}
          id={view === "screen" ? "screen-tile" : undefined}
        >
          {view === "screen" ? <MonitorIcon size={20} /> : <Avatar name={person} large />}
          <span>{view === "screen" ? "Screen" : person}</span>
        </div>
      );
    };
    return (
      <div className={`app mock${fullscreen ? " fullscreen" : ""}${fullscreen && chrome ? " chrome" : ""}`}>
          <header className="topbar" data-tauri-drag-region>
            <span className="topbar-title"><img className="name-mark" src={ramenUrl} alt="" /> Yatai</span>
            {notice || status ? <span className={`call-live${status && /microphone|could not|did not/i.test(status) ? " warn" : ""}`}>{notice ?? status}</span> : null}
            <button
              className="icon-quiet"
              type="button"
              aria-pressed={fullscreen}
              aria-label={fullscreen ? "Exit full screen" : "Full screen"}
              onClick={() => {
                const on = !fullscreen;
                setFullscreen(on);
                void invoke("set_fullscreen", { body: { on } });
              }}
            >
              {fullscreen ? <FullscreenExitIcon size={18} /> : <FullscreenIcon size={18} />}
            </button>
          </header>
          <div className="shell call-shell">
            {rail}
            <div className="workspace call">
              <div
                className="call-panel"
                ref={panelRef}
                onContextMenu={(event) => {
                  event.preventDefault();
                  placePop(event.clientX, event.clientY);
                  setDevicePane(null);
                  setMenu(sharing ? "hear" : "audio");
                }}
              >
                <div className="stage" id="stage" />
                <div className="call-chrome">
                  <div className={`center-face${showCenter ? " shown" : ""}`}>
                    <Avatar name={presenting ? mainPerson : (featured?.name || you?.name || name || "You")} large />
                  </div>
                  {volumeOpen && popAt ? (
                    <div className="voice-pop" role="dialog" aria-label={menu === "hear" ? "Screen audio" : "Voice"} style={{ left: popAt.x, top: popAt.y }} onContextMenu={(event) => { event.preventDefault(); event.stopPropagation(); }}>
                      <VolumeRow label={menu === "hear" ? "Audio" : "Voice"} value={menu === "hear" ? screen : voice} onChange={menu === "hear" ? changeScreen : changeVoice} />
                    </div>
                  ) : null}
                  {shareOpen ? (
                    <div className="menu-row" onContextMenu={(event) => event.stopPropagation()}>
                      <div className="mock-menu" role="dialog" aria-label="Share">
                        <button type="button" onClick={() => void showScreens()}>Select Screen <ChevronRightIcon size={18} /></button>
                        <button type="button" onClick={() => setMenu("stream")}>Stream Options <ChevronRightIcon size={18} /></button>
                      </div>
                      {menu === "screens" ? (
                        <div className="mock-menu" role="dialog" aria-label="Select a screen">
                          {picker && picker.length > 0 ? picker.map((choice) => (
                            <button key={choice.id} className="stack" type="button" onClick={() => void run(() => useScreen(choice.id))}>
                              <span>{choice.label}<small>{choice.width}x{choice.height}</small></span>
                            </button>
                          )) : <p>No screens found.</p>}
                        </div>
                      ) : null}
                      {menu === "stream" || menu === "resolution" || menu === "fps" || menu === "bitrate" ? (
                        <div className="mock-menu" role="dialog" aria-label="Stream options">
                          <button className="stack" type="button" onClick={() => setMenu("resolution")}><span>Resolution<small>{size.label.replace(" × ", "x")}</small></span><span className="chev"><ChevronRightIcon size={18} /></span></button>
                          <button className="stack" type="button" onClick={() => setMenu("fps")}><span>Framerate<small>{fps} Hz</small></span><span className="chev"><ChevronRightIcon size={18} /></span></button>
                          <button className="stack" type="button" onClick={() => setMenu("bitrate")}><span>Bitrate<small>{bitrate / 1000} Mbps</small></span><span className="chev"><ChevronRightIcon size={18} /></span></button>
                        </div>
                      ) : null}
                      {menu === "resolution" ? (
                        <div className="mock-menu" role="dialog" aria-label="Resolution">
                          {[...RESOLUTIONS].reverse().map((item) => {
                            const index = RESOLUTIONS.indexOf(item);
                            return (
                              <button key={item.label} type="button" className={index === resolution ? "picked" : ""} onClick={() => tune({ resolution: index })}>
                                <span>{item.label.replace(" × ", "x")}</span><i className="radio" />
                              </button>
                            );
                          })}
                        </div>
                      ) : null}
                      {menu === "fps" ? (
                        <div className="mock-menu" role="dialog" aria-label="Frame rate">
                          {[30, 60].map((value) => (
                            <button key={value} type="button" className={fps === value ? "picked" : ""} onClick={() => tune({ fps: value })}>
                              <span>{value} Hz</span><i className="radio" />
                            </button>
                          ))}
                        </div>
                      ) : null}
                      {menu === "bitrate" ? (
                        <div className="mock-menu" role="dialog" aria-label="Bitrate">
                          {BITRATES.map((value) => (
                            <button key={value} type="button" className={bitrate === value ? "picked" : ""} onClick={() => tune({ bitrate: value })}>
                              <span>{value / 1000} Mbps</span><i className="radio" />
                            </button>
                          ))}
                        </div>
                      ) : null}
                    </div>
                  ) : null}
                  <div
                    className={`tile-stack corner-${tileCorner}${tileAt ? " placed" : ""}${tileDragging ? " dragging" : ""}`}
                    style={tileAt ? { left: tileAt.x, top: tileAt.y } : undefined}
                    onTransitionEnd={(event) => {
                      if (event.target !== event.currentTarget) return;
                      if (event.propertyName !== "left" && event.propertyName !== "top") return;
                      if (!tileDrag.current) setTileAt(null);
                    }}
                    onContextMenu={(event) => event.stopPropagation()}
                    onPointerDown={(event) => {
                      if (event.button !== 0) return;
                      const origin = event.currentTarget.offsetParent;
                      if (!(origin instanceof HTMLElement)) return;
                      const originBox = origin.getBoundingClientRect();
                      const box = event.currentTarget.getBoundingClientRect();
                      tileDrag.current = {
                        dx: event.clientX - box.left,
                        dy: event.clientY - box.top,
                        sx: event.clientX,
                        sy: event.clientY
                      };
                      try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* pointer capture is unavailable for this event */ }
                      setTileDragging(true);
                      setTileAt({ x: box.left - originBox.left, y: box.top - originBox.top });
                    }}
                    onPointerMove={(event) => {
                      const drag = tileDrag.current;
                      const origin = event.currentTarget.offsetParent;
                      if (!drag || !(origin instanceof HTMLElement)) return;
                      const originBox = origin.getBoundingClientRect();
                      const maxX = originBox.width - event.currentTarget.offsetWidth - 4;
                      const maxY = originBox.height - event.currentTarget.offsetHeight - 4;
                      setTileAt({
                        x: Math.min(maxX, Math.max(4, event.clientX - originBox.left - drag.dx)),
                        y: Math.min(maxY, Math.max(4, event.clientY - originBox.top - drag.dy))
                      });
                    }}
                    onPointerUp={(event) => {
                      const drag = tileDrag.current;
                      const origin = event.currentTarget.offsetParent;
                      tileDrag.current = null;
                      if (!drag || !(origin instanceof HTMLElement)) {
                        setTileDragging(false);
                        return;
                      }
                      const originBox = origin.getBoundingClientRect();
                      const node = event.currentTarget;
                      const x = event.clientX - originBox.left - drag.dx;
                      const y = event.clientY - originBox.top - drag.dy;
                      const corner: TileCorner = `${y + node.offsetHeight / 2 < originBox.height / 2 ? "t" : "b"}${x + node.offsetWidth / 2 < originBox.width / 2 ? "l" : "r"}`;
                      const target = {
                        x: corner.endsWith("l") ? 12 : originBox.width - node.offsetWidth - 12,
                        y: corner.startsWith("t") ? 12 : originBox.height - node.offsetHeight - 12
                      };
                      setTileCorner(corner);
                      setTileDragging(false);
                      const moved = Math.hypot(event.clientX - drag.sx, event.clientY - drag.sy);
                      if (moved < 6) {
                        setTileAt(null);
                        if (presenting) {
                          const hit = (event.target as Element).closest<HTMLElement>("[data-tile]");
                          const slot = hit?.dataset.tile;
                          if (slot === "a" || slot === "b") {
                            setSlots((current) => ({ ...current, main: current[slot], [slot]: current.main }));
                          }
                        }
                        return;
                      }
                      requestAnimationFrame(() => setTileAt(target));
                    }}
                  >
                    <div className={`tile-slot${presenting ? "" : " shut"}`}>
                      <div className="tile-clip">{renderTile(slots.a, "a")}</div>
                    </div>
                    <div className="tile-slot">
                      <div className="tile-clip">
                        {presenting ? renderTile(slots.b, "b") : (
                          <div className={`face-tile you-tile${you?.speaking && !muted ? " speaking" : ""}`}>
                            <Avatar name={you?.name || name || "You"} large />
                            <span>{you?.name || name || "You"}</span>
                          </div>
                        )}
                      </div>
                    </div>
                  </div>
                  <div className="name-pill">
                    {pillOnScreen ? <MonitorIcon size={16} /> : null}
                    {pillMuted ? <span className="muted-mic"><MicOffIcon size={16} /></span> : null}
                    <strong>{pillName}</strong>
                  </div>
                  <div className="call-dock" onContextMenu={(event) => event.stopPropagation()}>
                    <div className="controls">
                      <div className={`tool${muted ? " on" : ""}${menu === "devices" ? " open" : ""}`}>
                        <button
                          type="button"
                          aria-pressed={muted}
                          aria-label={muted ? "Unmute" : "Mute"}
                          onClick={() => {
                            primeAudio();
                            const next = !muted;
                            void run(async () => {
                              await invoke("set_muted", { body: { muted: next } });
                              setMuted(next);
                              if (next) playMute();
                              else playUnmute();
                            });
                          }}
                        >
                          {muted ? <MicOffIcon size={18} /> : <MicIcon size={18} />}
                        </button>
                        <button className={`caret${menu === "devices" ? " open" : ""}`} type="button" aria-label="Microphone settings" onClick={() => { setDevicePane(null); setMenu(menu === "devices" ? null : "devices"); }}>
                          <ChevronIcon size={16} />
                        </button>
                        {menu === "devices" ? (
                          <div className="pop-anchor" onContextMenu={(event) => event.stopPropagation()}>
                            <div className="mock-menu" role="dialog" aria-label="Microphone settings" ref={micMenuRef}>
                              <button className="stack" type="button" data-pane="input" onClick={() => setDevicePane(devicePane === "input" ? null : "input")}>
                                <span>Input Device<small>{inputName}</small></span>
                                <span className="chev"><ChevronRightIcon size={18} /></span>
                              </button>
                              <button className="stack" type="button" data-pane="output" onClick={() => setDevicePane(devicePane === "output" ? null : "output")}>
                                <span>Output Device<small>{outputName}</small></span>
                                <span className="chev"><ChevronRightIcon size={18} /></span>
                              </button>
                              <button type="button" onClick={() => pickNoise(!noise)}>
                                <span>Noise Suppression</span>
                                <i className={`check${noise ? " on" : ""}`}>{noise ? "✓" : ""}</i>
                              </button>
                            </div>
                            {devicePane ? (
                              <div className="mock-menu pane" role="dialog" aria-label={devicePane === "input" ? "Input Device" : "Output Device"} style={{ marginTop: paneShift }}>
                                <button type="button" className={(devicePane === "input" ? inputId : outputId) === "" ? "picked" : ""} onClick={() => devicePane === "input" ? pickInput("") : pickOutput("")}>
                                  <span>System default</span><i className="radio" />
                                </button>
                                {(devicePane === "input" ? inputs : outputs).map((device) => (
                                  <button key={device.id} type="button" className={(devicePane === "input" ? inputId : outputId) === device.id ? "picked" : ""} onClick={() => devicePane === "input" ? pickInput(device.id) : pickOutput(device.id)}>
                                    <span>{device.name}</span><i className="radio" />
                                  </button>
                                ))}
                              </div>
                            ) : null}
                          </div>
                        ) : null}
                      </div>
                      <div className="tool">
                        <button type="button" aria-label="Camera"><VideoIcon size={18} /></button>
                        <button className="caret" type="button" aria-label="Camera" tabIndex={-1}><ChevronIcon size={16} /></button>
                      </div>
                      <div className={`tool${deaf ? " on" : ""}${volumeOpen ? " open" : ""}`}>
                        <button type="button" aria-pressed={deaf} aria-label={deaf ? "Undeafen" : "Deafen"} onClick={toggleDeaf}>
                          {deaf ? <HeadphonesOffIcon size={18} /> : <HeadphonesIcon size={18} />}
                        </button>
                        <button className={`caret${volumeOpen ? " open" : ""}`} type="button" aria-label="Volume" onClick={(event) => {
                          if (volumeOpen) { setMenu(null); return; }
                          const box = event.currentTarget.getBoundingClientRect();
                          placePop(box.left, box.top - 48);
                          setMenu(sharing ? "hear" : "audio");
                        }}>
                          <ChevronIcon size={16} />
                        </button>
                      </div>
                      <span className="control-split" />
                      <div className={`tool${sharing ? " live" : ""}${shareOpen ? " open" : ""}`}>
                        <button type="button" aria-pressed={sharing} aria-label={sharing ? "Stop sharing" : "Share screen"} onClick={() => void run(shareScreen)}>
                          {sharing ? <StopShareIcon size={18} /> : <ShareIcon size={18} />}
                        </button>
                        <button className={`caret${shareOpen ? " open" : ""}`} type="button" aria-label="Share settings" onClick={() => setMenu(menu === "share" ? null : "share")}>
                          <ChevronIcon size={16} />
                        </button>
                      </div>
                    </div>
                    <button className="hangup" type="button" disabled={busy} aria-label="Leave" onClick={() => { primeAudio(); void run(() => invoke("leave")); }}>
                      <PhoneIcon size={18} />
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </div>
      </div>
    );
  }

  if (!session) {
    const days = Array.from({ length: 31 }, (_, index) => String(index + 1));
    const months = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    const years = Array.from({ length: 80 }, (_, index) => String(2012 - index));
    const loginReady = authEmail.trim().length > 0 && authPass.length > 0;
    const registerReady = authEmail.trim().length > 0 && authUser.trim().length > 0 && authPass.length > 0 && dobDay && dobMonth && dobYear;
    return (
      <div className="auth">
        <div className="auth-card">
          <form
            className="auth-form"
            onSubmit={(event) => {
              event.preventDefault();
              if (authBusy) return;
              if (authMode === "login" ? !loginReady : !registerReady) return;
              setAuthBusy(true);
              setAuthNote("");
              const body = authMode === "login"
                ? { action: "login", login: authEmail.trim(), password: authPass }
                : { action: "register", email: authEmail.trim(), username: authUser.trim(), password: authPass, day: dobDay, month: dobMonth, year: dobYear };
              void invoke<{ username: string; session: string }>("account", { body })
                .then((account) => rememberAccount(account))
                .catch((err: unknown) => setAuthNote(accountError(err)))
                .finally(() => setAuthBusy(false));
            }}
          >
            <h1>{authMode === "login" ? "Hey, there!" : "Create an account"}</h1>
            {authMode === "login" ? <p className="auth-sub">Great to see you, again!</p> : null}
            <label>
              <span>{authMode === "login" ? "Email or Username" : "Email"} <i>*</i></span>
              <span className="auth-field">
                <input value={authEmail} autoComplete="username" onChange={(event) => setAuthEmail(event.target.value)} />
              </span>
            </label>
            {authMode === "register" ? (
              <label>
                <span>Username <i>*</i></span>
                <span className="auth-field">
                  <input value={authUser} autoComplete="nickname" onChange={(event) => setAuthUser(event.target.value)} />
                </span>
              </label>
            ) : null}
            <label>
              <span>Password <i>*</i></span>
              <span className="auth-field">
                <input type={showPassword ? "text" : "password"} value={authPass} autoComplete={authMode === "login" ? "current-password" : "new-password"} onChange={(event) => setAuthPass(event.target.value)} />
                <button type="button" aria-label={showPassword ? "Hide password" : "Show password"} onClick={() => setShowPassword((current) => !current)}>
                  {showPassword ? <VisibilityOffIcon size={18} /> : <VisibilityIcon size={18} />}
                </button>
              </span>
            </label>
            {authMode === "login" ? (
              <button className="auth-link" type="button" onClick={() => setAuthNote("Password reset isn't available yet.")}>Forget your password?</button>
            ) : (
              <label>
                <span>Date of Birth <i>*</i></span>
                <span className="dob-row">
                  <span className="auth-field">
                    <select value={dobDay} onChange={(event) => setDobDay(event.target.value)} aria-label="Day">
                      <option value="">Day</option>
                      {days.map((day) => <option key={day} value={day}>{day}</option>)}
                    </select>
                    <ChevronIcon size={16} />
                  </span>
                  <span className="auth-field">
                    <select value={dobMonth} onChange={(event) => setDobMonth(event.target.value)} aria-label="Month">
                      <option value="">Month</option>
                      {months.map((month) => <option key={month} value={month}>{month}</option>)}
                    </select>
                    <ChevronIcon size={16} />
                  </span>
                  <span className="auth-field">
                    <select value={dobYear} onChange={(event) => setDobYear(event.target.value)} aria-label="Year">
                      <option value="">Year</option>
                      {years.map((year) => <option key={year} value={year}>{year}</option>)}
                    </select>
                    <ChevronIcon size={16} />
                  </span>
                </span>
              </label>
            )}
            <button className="auth-submit" type="submit" disabled={authBusy}>{authBusy ? "Please wait" : authMode === "login" ? "Log In" : "Create Account"}</button>
            {authNote ? <p className="auth-note">{authNote}</p> : null}
            {authMode === "login" ? (
              <p className="auth-switch">Need an account? <button type="button" onClick={() => { setAuthMode("register"); setAuthNote(""); }}>Register</button></p>
            ) : (
              <p className="auth-switch">Already have an account? <button type="button" onClick={() => { setAuthMode("login"); setAuthNote(""); }}>Log in</button></p>
            )}
          </form>
          <div className="auth-mark">
            <RamenMark size={150} />
            <strong>Yatai</strong>
          </div>
        </div>
      </div>
    );
  }

  const needle = query.trim().toLowerCase().replace(/^@/, "");
  const added = desk.friends.filter((friend) => friend.added);
  const friendRows = added.filter((friend) => !needle || friend.name.toLowerCase().includes(needle) || handleOf(friend.name).includes(needle));
  const direct = desk.threads.filter((item) => {
    const known = added.some((friend) => friend.name.toLowerCase() === item.name.toLowerCase());
    return known && (!needle || item.name.toLowerCase().includes(needle) || handleOf(item.name).includes(needle));
  });
  const selectedName = direct.some((item) => item.name === active) ? active : null;
  const selected = direct.find((item) => item.name === selectedName) ?? null;
  const thread = selected ? desk.threads.find((item) => item.name.toLowerCase() === selected.name.toLowerCase()) : null;
  const already = new Set(added.map((friend) => friend.name.toLowerCase()));
  const suggestions = foundPeople.filter((person) => !already.has(person.toLowerCase()));

  return (
    <div className="app mock">
          <header className="topbar" data-tauri-drag-region>
            <span className="topbar-title"><img className="name-mark" src={ramenUrl} alt="" /> Yatai</span>
          </header>
          <div className="shell">
            {rail}
            <main className="workspace">
                <div className="messages">
                  <div className="chat-board">
                    <section className="dm-col">
                      <label className="search">
                        <PersonSearchIcon size={18} />
                        <input id="thread-search" placeholder="Find your friend" value={query} onChange={(event) => setQuery(event.target.value)} />
                      </label>
                      <hr className="dm-rule" />
                      <div className="thread-scroll">
                        <p className="dm-label">Direct Messages</p>
                        {needle && direct.length === 0 ? <p className="empty">No matches.</p> : null}
                        {direct.map((item) => {
                          const count = item.notes.length;
                          const badge = item.name === selectedName || count === 0 ? "" : count > 9 ? "9+" : String(count);
                          return (
                            <button className={`thread-row${item.name === selectedName ? " active" : ""}`} type="button" key={item.name} onClick={() => setActive(item.name)}>
                              <Avatar name={item.name} />
                              <span className="thread-copy">
                                <strong>{item.name}</strong>
                              </span>
                              {badge ? <span className="badge">{badge}</span> : null}
                            </button>
                          );
                        })}
                        <hr className="dm-rule" />
                        <p className="dm-label">Friends</p>
                        {needle && friendRows.length === 0 ? <p className="empty">No matches.</p> : null}
                        {friendRows.map((friend) => (
                          <button className="thread-row" type="button" key={friend.name} onClick={() => openFriend(friend)}>
                            <Avatar name={friend.name} />
                            <span className="thread-copy"><strong>{friend.name}</strong></span>
                          </button>
                        ))}
                      </div>
                      <div className="you dm-you">
                        <Avatar name={name || "You"} />
                        <strong>{name || "Your name"}</strong>
                      </div>
                    </section>
                    <section className="thread-pane">
                      {selected ? (
                        <>
                          <header className="thread-top">
                            <Avatar name={selected.name} online={online.has(selected.name)} />
                            <span className="person-line">
                              <strong>{selected.name}</strong>
                            </span>
                            <span className="thread-tools">
                              <button type="button" aria-label="Call"><PhoneIcon size={18} /></button>
                            </span>
                          </header>
                          <div className="transcript">
                            {noteDays(thread?.notes ?? []).map((group) => (
                              <div key={group.day}>
                                <div className="day"><span /><em>{group.day}</em><span /></div>
                                {group.notes.map((mail) => {
                                  const mine = mail.author.trim().toLowerCase() === name.trim().toLowerCase();
                                  return (
                                    <article className={`bubble${mine ? " mine" : ""}`} key={mail.id}>
                                      {mine ? null : <Avatar name={mail.author} small />}
                                      <div>
                                        <p>{mail.text}</p>
                                        <time>{clock(mail.at)}</time>
                                      </div>
                                      {mine ? <Avatar name={mail.author} small /> : null}
                                    </article>
                                  );
                                })}
                              </div>
                            ))}
                          </div>
                          <div className="composer">
                            <form onSubmit={(event) => { event.preventDefault(); sendNote(selected.name, selected.room); }}>
                              <input aria-label={`Message ${selected.name}`} placeholder={`Message to ${selected.name}`} value={draft} onChange={(event) => setDraft(event.target.value)} />
                              <button className="emoji" type="button" aria-label="Add an emoji" onClick={() => setDraft((current) => `${current}✨`)}><SmileIcon size={18} /></button>
                              <button className="send" type="submit" aria-label="Save note"><SendIcon size={18} /></button>
                            </form>
                          </div>
                        </>
                      ) : (
                        <div className="home-empty">
                          <button className="add-friend" type="button" onClick={() => setFriendOpen((open) => !open)}>
                            <PersonAddIcon size={18} /> Add Friend
                          </button>
                          {friendOpen ? (
                            <div className="friend-pop" role="dialog" aria-label="Search for a friend">
                              <label className="search">
                                <PersonSearchIcon size={18} />
                                <input placeholder="Find someone online" value={friendQuery} onChange={(event) => setFriendQuery(event.target.value)} />
                              </label>
                              {suggestions.map((hit) => (
                                <div className="friend-hit" key={hit}>
                                  <Avatar name={hit} />
                                  <span>
                                    <strong>{hit}</strong>
                                    <small>{handleOf(hit)}</small>
                                  </span>
                                  <button type="button" aria-label={`Add ${hit}`} onClick={() => addFoundFriend(hit)}><PersonAddIcon size={18} /></button>
                                </div>
                              ))}
                              {friendQuery.trim().replace(/^@/, "").length >= 2 && suggestions.length === 0 ? <p className="empty">No one online matches that.</p> : null}
                            </div>
                          ) : null}
                          <div className="chat-empty">
                            <img src={ramenUrl} alt="" />
                            <p>Ramen may have originated from China. Scholars believe that “shina shoba” (“Chinese style” noodle soup) was imported to Japan between the 17th and 20th centuries.</p>
                          </div>
                        </div>
                      )}
                    </section>
                  </div>
                </div>
            </main>
          </div>
        </div>
      );
}

function Rail({ prefsOpen, onPrefs, onTheme, onLogOut, dark }: { prefsOpen: boolean; onPrefs: () => void; onTheme: () => void; onLogOut: () => void; dark: boolean }) {
  return (
    <aside className="sidebar">
      <div className="brand-mark" aria-label="Yatai">
        <img src={ramenUrl} alt="" />
      </div>
      <button className="nav-btn active" type="button" aria-label="Messages">
        <ChatBubbleIcon size={20} />
      </button>
      <button className="nav-btn plain" type="button" aria-label="Call">
        <PhoneIcon size={20} />
      </button>
      <button className={`nav-btn plain rail-end${prefsOpen ? " active" : ""}`} type="button" aria-label="Settings" onClick={onPrefs}>
        <SettingsIcon size={20} />
      </button>
      {prefsOpen ? (
        <div className="mock-menu rail-pop" role="dialog" aria-label="Settings">
          <button type="button" onClick={onTheme}>{dark ? "Light theme" : "Dark theme"}</button>
          <button type="button" onClick={onLogOut}>Log out</button>
        </div>
      ) : null}
    </aside>
  );
}

      function VolumeRow({label, value, onChange}: {label: string; value: number; onChange: (value: number) => void }) {
  return (
      <label className="slider-card">
        <span>{label}</span>
        <input
          type="range"
          min={0}
          max={200}
          step={5}
          value={value}
          aria-label={label === "Voice" ? "Other person's volume" : "Screen audio volume"}
          onChange={(event) => onChange(Number(event.target.value))}
        />
        <strong>{value}%</strong>
      </label>
      );
}

      function accountError(err: unknown) {
  if (typeof err === "string" && err.trim()) return err;
      return "Could not reach the account service.";
}

      function handleOf(name: string) {
  return `@${name.toLowerCase().replace(/[^a-z0-9]+/g, "")}`;
}

      function clock(at: number) {
  return new Date(at).toLocaleTimeString([], {hour: "numeric", minute: "2-digit" }).replace(" AM", " am").replace(" PM", " pm");
}

      function dayTitle(at: number) {
  const date = new Date(at);
      const months = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
      return `${months[date.getMonth()]} ${date.getDate()}, ${date.getFullYear()}`;
}

      function noteDays(notes: Mail[]) {
  const groups: {day: string; notes: Mail[] }[] = [];
      for (const mail of notes) {
    const day = dayTitle(mail.at);
      const last = groups[groups.length - 1];
      if (last && last.day === day) last.notes.push(mail);
      else groups.push({day, notes: [mail] });
  }
      return groups;
}

      function Avatar({name, online = false, large = false, small = false}: {name: string; online?: boolean; large?: boolean; small?: boolean }) {
  return (
      <span className={`avatar${large ? " lg" : ""}${small ? " sm" : ""}`} style={{ background: faceColor(name) }}>
        {initials(name)}
        {online ? <i className="presence on" /> : null}
      </span>
      );
}

