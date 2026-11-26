import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ago, faceColor, initials, loadDesk, saveDesk, type Desk, type Friend } from "./desk";
import {
  AddIcon,
  ArrowIcon,
  ChevronIcon,
  FriendsIcon,
  FullscreenExitIcon,
  FullscreenIcon,
  HeadphonesIcon,
  HomeIcon,
  LeaveIcon,
  MessagesIcon,
  MicIcon,
  MicOffIcon,
  MonitorIcon,
  PhoneIcon,
  RecentsIcon,
  SearchIcon,
  SendIcon,
  SettingsIcon,
  ShareIcon,
  StopIcon
} from "./icons";
import { playEnter, playExit, playJoined, playLeft, playMute, playUnmute, primeAudio } from "./sounds";

type Note =
  | { kind: "status"; text: string }
  | { kind: "joined" }
  | { kind: "left" }
  | { kind: "sharing"; on: boolean }
  | { kind: "people"; people: Person[] };

type Person = { name: string; local: boolean; speaking: boolean };

type Page = "home" | "friends" | "messages" | "recents" | "settings";
type Dialog = "create" | "join" | "friend" | null;
type Menu = "share" | "audio" | null;
type FriendTab = "online" | "all" | "requests";

const RESOLUTIONS = [
  { width: 1280, height: 720, label: "1280 × 720" },
  { width: 1920, height: 1080, label: "1920 × 1080" },
  { width: 2560, height: 1440, label: "2560 × 1440" }
];

const BITRATES = [8000, 12000, 20000, 40000, 80000, 100000];

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

const NAV: { id: Page; label: string; icon: typeof HomeIcon }[] = [
  { id: "home", label: "Home", icon: HomeIcon },
  { id: "friends", label: "Friends", icon: FriendsIcon },
  { id: "messages", label: "Messages", icon: MessagesIcon },
  { id: "recents", label: "Recents", icon: RecentsIcon },
  { id: "settings", label: "Settings", icon: SettingsIcon }
];

export function App() {
  const [theme, setTheme] = useState<"light" | "dark">("light");
  const [name, setName] = useState("");
  const [room, setRoom] = useState("");
  const [invite, setInvite] = useState("");
  const [phase, setPhase] = useState<"home" | "joining" | "call">("home");
  const [page, setPage] = useState<Page>("home");
  const [status, setStatus] = useState("");
  const [sharing, setSharing] = useState(false);
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
  const [dialog, setDialog] = useState<Dialog>(null);
  const [menu, setMenu] = useState<Menu>(null);
  const [friendTab, setFriendTab] = useState<FriendTab>("all");
  const [query, setQuery] = useState("");
  const [active, setActive] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [friendName, setFriendName] = useState("");
  const [friendRoom, setFriendRoom] = useState("");
  const sharingRef = useRef(false);
  const remotesRef = useRef<Set<string> | null>(null);
  const noticeTimer = useRef(0);
  const roomRef = useRef(room);
  const pendingShare = useRef(false);
  const heard = useRef({ voice: 100, screen: 100 });
  const shareRef = useRef<() => Promise<void>>(async () => {});
  roomRef.current = room;

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
    void invoke("remember", { body: saved(next) }).catch(() => {});
  }

  function loadDevices() {
    void invoke<Gear[]>("inputs").then(setInputs).catch(() => {});
    void invoke<Gear[]>("outputs").then(setOutputs).catch(() => {});
  }

  function pickInput(id: string) {
    setInputId(id);
    persist({ input: id });
    void invoke("set_input", { body: { id } }).catch(() => {});
  }

  function pickOutput(id: string) {
    setOutputId(id);
    persist({ output: id });
    void invoke("set_output", { body: { id } }).catch(() => {});
  }

  function pickNoise(on: boolean) {
    setNoise(on);
    persist({ noise: on });
    void invoke("set_noise", { body: { on } }).catch(() => {});
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
        { name: "malubulu", room: "mycord-room", at: now - 3_600_000 },
        { name: "Divyank Shandilya", room: "project-sync", at: now - 7_200_000 },
        { name: "beat_n", room: "game-night", at: now - 86_400_000 }
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
    setActive("souvlakee");
    if (preview === "call") {
      setPhase("call");
      setPeople([
        { name: "Avyukt", local: true, speaking: false },
        { name: "souvlakee", local: false, speaking: true }
      ]);
      setStatus("In the room");
    } else if (preview === "messages" || preview === "friends" || preview === "recents" || preview === "settings") {
      setPage(preview);
    }
  }, []);

  useEffect(() => {
    void invoke<Profile>("profile")
      .then((stored) => {
        if (new URLSearchParams(window.location.search).has("preview") && import.meta.env.DEV) return;
        setName(stored.name);
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
        void invoke("set_noise", { body: { on: suppression } }).catch(() => {});
      })
      .catch(() => {});
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
        sharingRef.current = false;
        remotesRef.current = null;
        setMuted(false);
        setDeaf(false);
        setFullscreen(false);
        setNotice(null);
        setMenu(null);
        setStatus("You left the room.");
        setBusy(false);
        playLeft();
        void invoke("set_fullscreen", { body: { on: false } });
      } else if (note.kind === "sharing") {
        setSharing(note.on);
        sharingRef.current = note.on;
        setStatus(note.on ? "Sharing your screen." : "");
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
          commit((current) => {
            let friends = current.friends;
            for (const person of remotes) {
              friends = [{ name: person.name, room: roomRef.current, at: Date.now() }, ...friends.filter((friend) => friend.name !== person.name)];
            }
            return {
              ...current,
              friends: friends.slice(0, 40),
              recents: current.recents.map((item, index) => (index === 0 ? { ...item, people: note.people.length } : item))
            };
          });
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
    if (phase !== "call") return;
    const node = document.getElementById("stage");
    if (!node) return;
    const post = () => {
      if (picker) {
        void invoke("stage_rect", { body: { x: 0, y: 0, width: 0, height: 0, visible: false } }).catch(() => {});
        return;
      }
      const rect = node.getBoundingClientRect();
      void invoke("stage_rect", {
        body: { x: rect.x, y: rect.y, width: rect.width, height: rect.height, visible: true }
      }).catch(() => {});
    };
    post();
    const observer = new ResizeObserver(post);
    observer.observe(node);
    window.addEventListener("resize", post);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", post);
      void invoke("stage_rect", { body: { x: 0, y: 0, width: 0, height: 0, visible: false } }).catch(() => {});
    };
  }, [phase, picker, fullscreen, chrome]);

  useEffect(() => {
    if (phase !== "call") return;
    void invoke("set_voice", { body: { percent: voice } }).catch(() => {});
    void invoke("set_screen", { body: { percent: screen } }).catch(() => {});
  }, [phase, voice, screen]);

  useEffect(() => {
    if (!menu) return;
    const close = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Element && target.closest(".tool, .strip")) return;
      setMenu(null);
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
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPage("messages");
        document.getElementById("thread-search")?.focus();
      }
      if (event.key === "Escape") setDialog(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
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

  async function shareScreen() {
    if (sharingRef.current) {
      await invoke("stop_share");
      return;
    }
    let found: ScreenChoice[] = [];
    try {
      found = await invoke<ScreenChoice[]>("screens");
    } catch {
      found = [];
    }
    if (found.length > 1) {
      setPicker(found);
      return;
    }
    const next = found[0]?.id ?? null;
    setDisplayId(next);
    await invoke("share", { body: { ...picture(), displayId: next } });
  }
  shareRef.current = shareScreen;

  function joinRoom(nextRoom: string, share = false) {
    const nextName = name.trim();
    const target = nextRoom.trim();
    if (!nextName || !target) {
      setDialog("join");
      setStatus("Add your name and a room.");
      return;
    }
    primeAudio();
    pendingShare.current = share;
    setRoom(target);
    setDialog(null);
    setPhase("joining");
    setStatus("Joining the room.");
    persist({ name: nextName, room: target });
    void run(() => invoke("join", { body: { name: nextName, room: target, invite: invite.trim() ? invite.trim() : null } }), false);
  }

  function addFriend() {
    const nextName = friendName.trim();
    const target = friendRoom.trim();
    if (!nextName || !target) return;
    commit((current) => ({
      ...current,
      friends: [{ name: nextName, room: target, at: Date.now() }, ...current.friends.filter((friend) => friend.name !== nextName)].slice(0, 40)
    }));
    setFriendName("");
    setFriendRoom("");
    setDialog(null);
    setActive(nextName);
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
    void invoke("tune", { body: { width: size.width, height: size.height, fps: fpsNext, bitrateKbps: bitrateNext } }).catch(() => {});
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
  const visibleFriends = desk.friends.filter((friend) => friendTab !== "online" || online.has(friend.name));

  if (phase === "call") {
    const others = people.filter((person) => !person.local);
    const featured = others.find((person) => person.speaking) ?? others[0];
    const you = people.find((person) => person.local);
    const talking = Boolean(featured && featured.speaking);
    return (
      <div className={`app${fullscreen ? " fullscreen" : ""}${fullscreen && chrome ? " chrome" : ""}`}>
        <div className="call">
          <header className="call-top">
            <div className="brand">
              <i>//</i> MyCord
            </div>
            <div className="call-room">
              <FriendsIcon size={16} />
              <strong>{room || "Room"}</strong>
              <span>{Math.max(people.length, 1)} {Math.max(people.length, 1) === 1 ? "person" : "people"}</span>
            </div>
            <div className={`call-live${status && /microphone|could not|did not/i.test(status) ? " warn" : ""}`}>{notice ?? status}</div>
          </header>
          <div className="stage" id="stage">
            <div className="stage-copy">{sharing ? "Sharing your screen." : "Waiting for a screen."}</div>
          </div>
          {menu === "audio" ? (
            <div className="strip" role="dialog" aria-label="Audio boost">
              <label>
                Voice
                <input type="range" min={0} max={200} step={5} value={voice} aria-label="Other person's volume" onChange={(event) => changeVoice(Number(event.target.value))} />
              </label>
              <strong className="boost-read">{voice}%</strong>
              <label>
                Screen
                <input type="range" min={0} max={200} step={5} value={screen} aria-label="Screen audio volume" onChange={(event) => changeScreen(Number(event.target.value))} />
              </label>
              <strong className="boost-read">{screen}%</strong>
              <DeviceSelect label="Microphone" value={inputId} devices={inputs} onChange={pickInput} />
              <DeviceSelect label="Speakers" value={outputId} devices={outputs} onChange={pickOutput} />
              <NoiseSwitch on={noise} onChange={pickNoise} />
            </div>
          ) : null}
          {menu === "share" ? (
            <div className="strip" role="dialog" aria-label="Screen share settings">
              <label>
                Resolution
                <select aria-label="Resolution" value={resolution} onChange={(event) => tune({ resolution: Number(event.target.value) })}>
                  {RESOLUTIONS.map((item, index) => (
                    <option key={item.label} value={index}>{item.label}</option>
                  ))}
                </select>
              </label>
              <label>
                Framerate
                <select aria-label="Frame rate" value={fps} onChange={(event) => tune({ fps: Number(event.target.value) })}>
                  <option value={30}>30 Hz</option>
                  <option value={60}>60 Hz</option>
                </select>
              </label>
              <label>
                Bitrate
                <select aria-label="Bitrate" value={bitrate} onChange={(event) => tune({ bitrate: Number(event.target.value) })}>
                  {BITRATES.map((value) => (
                    <option key={value} value={value}>{value / 1000} Mbps</option>
                  ))}
                </select>
              </label>
            </div>
          ) : null}
          <div className="call-dock">
            <button
              className={`who${talking ? " speaking" : ""}`}
              type="button"
              onContextMenu={(event) => {
                event.preventDefault();
                setMenu("audio");
              }}
            >
              <Avatar name={featured?.name || name || "You"} online={Boolean(featured)} />
              <span className="person-line">
                <strong>{featured?.name || "Just you"}</strong>
                <span className="bars" aria-hidden="true">
                  <i /><i /><i /><i />
                </span>
              </span>
            </button>
            <div className="controls">
              <div className={`tool${muted ? " muted" : ""}`} onPointerDown={(event) => event.stopPropagation()}>
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
                  {muted ? <MicOffIcon /> : <MicIcon />}
                </button>
                <button className="caret" type="button" aria-label="Voice volume" onClick={() => setMenu(menu === "audio" ? null : "audio")}>
                  <ChevronIcon size={18} />
                </button>
              </div>
              <div className={`tool${deaf ? " muted" : ""}`} onPointerDown={(event) => event.stopPropagation()}>
                <button type="button" aria-pressed={deaf} aria-label={deaf ? "Hear the room" : "Silence the room"} onClick={toggleDeaf}>
                  <HeadphonesIcon />
                </button>
              </div>
              <div className={`tool${sharing ? " on" : ""}`} onPointerDown={(event) => event.stopPropagation()}>
                <button type="button" aria-pressed={sharing} onClick={() => void run(shareScreen)}>
                  {sharing ? <StopIcon size={18} /> : <ShareIcon size={18} />}
                  <span className="share-label">{sharing ? "Stop sharing" : "Share screen"}</span>
                </button>
                <button className="caret" type="button" aria-label="Share settings" onClick={() => setMenu(menu === "share" ? null : "share")}>
                  <ChevronIcon size={18} />
                </button>
              </div>
              <div className="tool">
                <button
                  type="button"
                  aria-pressed={fullscreen}
                  aria-label={fullscreen ? "Exit full screen" : "Full screen"}
                  onClick={() => {
                    const on = !fullscreen;
                    setFullscreen(on);
                    void invoke("set_fullscreen", { body: { on } });
                  }}
                >
                  {fullscreen ? <FullscreenExitIcon /> : <FullscreenIcon />}
                </button>
              </div>
              <button className="danger" type="button" disabled={busy} onClick={() => { primeAudio(); void run(() => invoke("leave")); }}>
                <LeaveIcon size={18} /> Leave
              </button>
            </div>
            <div className={`you-tile${you?.speaking && !muted ? " speaking" : ""}`}>
              <Avatar name={you?.name || name || "You"} large />
              You
            </div>
          </div>
        </div>
        {picker ? (
          <div className="picker" role="dialog" aria-modal="true" aria-label="Choose a screen">
            <div className="picker-card">
              <h2>Choose a screen</h2>
              {picker.map((choice) => (
                <button
                  className="room"
                  key={choice.id}
                  type="button"
                  onClick={() => {
                    const next = choice.id;
                    setPicker(null);
                    setDisplayId(next);
                    void run(() => invoke("share", { body: { ...picture(), displayId: next } }));
                  }}
                >
                  <span className="room-mark"><MonitorIcon size={18} /></span>
                  <span className="person-line">
                    <strong>{choice.label}</strong>
                    <span>{choice.width} × {choice.height}</span>
                  </span>
                </button>
              ))}
              <button className="outline" type="button" onClick={() => setPicker(null)}>Cancel</button>
            </div>
          </div>
        ) : null}
      </div>
    );
  }

  const threadNames = conversationList(desk, query);
  const selectedName = threadNames.some((item) => item.name === active) ? active : threadNames[0]?.name ?? null;
  const selected = threadNames.find((item) => item.name === selectedName) ?? null;
  const thread = desk.threads.find((item) => item.name === selectedName);

  return (
    <div className="app">
      <div className="shell">
        <aside className="sidebar">
          <div className="brand"><i>//</i> <span>MyCord</span></div>
          <nav className="nav-list" aria-label="Sections">
            {NAV.map((item) => (
              <button key={item.id} className={`nav-btn${page === item.id ? " active" : ""}`} type="button" onClick={() => setPage(item.id)}>
                <item.icon size={20} />
                <span>{item.label}</span>
              </button>
            ))}
          </nav>
          <button className="you" type="button" onClick={() => setPage("settings")}>
            <Avatar name={name || "You"} online />
            <span className="person-line">
              <strong>{name || "Your name"}</strong>
              <span>Online</span>
            </span>
          </button>
        </aside>
        <main className="workspace">
          {page === "home" ? (
            <div className="home">
              <div className="home-main">
                <section className="hero">
                  <h1>Connect, watch, and build together.</h1>
                  <p>A simple space to call and share your screen with the people you care about.</p>
                  <div className="hero-actions">
                    <button className="cream" type="button" onClick={() => setDialog("create")}><AddIcon size={18} /> Create a Room</button>
                    <button className="outline" type="button" onClick={() => setDialog("join")}>Join a Room</button>
                  </div>
                  {status ? <p className="notice">{phase === "joining" ? "Joining the room." : status}</p> : null}
                </section>
                <section>
                  <div className="section-head">
                    <h2>Recent Rooms</h2>
                    <button className="text-btn" type="button" onClick={() => setPage("recents")}>View all</button>
                  </div>
                  <div className="room-list">
                    {desk.recents.length === 0 ? <p className="empty">Rooms you open will stay here.</p> : null}
                    {desk.recents.slice(0, 3).map((item) => (
                      <button className="room" type="button" key={item.room} onClick={() => joinRoom(item.room)}>
                        <span className="room-mark"><MonitorIcon size={18} /></span>
                        <span className="person-line">
                          <strong>{item.room}</strong>
                          <span>{item.people} {item.people === 1 ? "person" : "people"} · {ago(item.at)}</span>
                        </span>
                        <ArrowIcon size={18} />
                      </button>
                    ))}
                  </div>
                </section>
              </div>
              <div className="side-col">
                <section className="panel">
                  <div className="panel-head">
                    <h2>Friends</h2>
                    <button className="text-btn" type="button" onClick={() => setDialog("friend")}>Add Friend</button>
                  </div>
                  <FriendTabs tab={friendTab} setTab={setFriendTab} online={online.size} />
                  <FriendRows friends={visibleFriends} tab={friendTab} online={online} onCall={(friend) => joinRoom(friend.room)} onShare={(friend) => joinRoom(friend.room, true)} />
                </section>
                <section className="panel">
                  <div className="panel-head"><h2>Activity</h2></div>
                  <div className="activity-list">
                    {desk.friends.length === 0 ? <p className="empty">When someone joins a room with you, they show up here.</p> : null}
                    {desk.friends.slice(0, 4).map((friend) => (
                      <div className="activity" key={friend.name}>
                        <Avatar name={friend.name} online={online.has(friend.name)} />
                        <span className="activity-copy">
                          <strong>{friend.name}</strong>
                          <span>{online.has(friend.name) ? "In a room with you" : `Last room ${friend.room} · ${ago(friend.at)}`}</span>
                        </span>
                      </div>
                    ))}
                  </div>
                </section>
              </div>
            </div>
          ) : null}
          {page === "friends" ? (
            <div className="page">
              <div className="page-head">
                <h1>Friends</h1>
                <button className="outline" type="button" onClick={() => setDialog("friend")}><AddIcon size={18} /> Add Friend</button>
              </div>
              <FriendTabs tab={friendTab} setTab={setFriendTab} online={online.size} />
              <div className="friend-list">
                <FriendRows friends={visibleFriends} tab={friendTab} online={online} onCall={(friend) => joinRoom(friend.room)} onShare={(friend) => joinRoom(friend.room, true)} />
              </div>
            </div>
          ) : null}
          {page === "messages" ? (
            <div className="messages">
              <section>
                <div className="inbox-head">
                  <div className="section-head">
                    <h2>Messages</h2>
                    <button className="icon-quiet" type="button" aria-label="Add a friend" onClick={() => setDialog("friend")}><AddIcon size={18} /></button>
                  </div>
                  <label className="search">
                    <SearchIcon size={16} />
                    <input id="thread-search" placeholder="Search conversations" value={query} onChange={(event) => setQuery(event.target.value)} />
                    <kbd>⌘K</kbd>
                  </label>
                </div>
                <div className="thread-scroll">
                  {threadNames.length === 0 ? <p className="empty">Add a friend, then open a room together.</p> : null}
                  {threadNames.map((item) => (
                    <button className={`thread-row${item.name === selectedName ? " active" : ""}`} type="button" key={item.name} onClick={() => setActive(item.name)}>
                      <Avatar name={item.name} online={online.has(item.name)} />
                      <span className="thread-copy">
                        <strong>{item.name}</strong>
                        <span>{item.preview || item.room || "No notes yet"}</span>
                      </span>
                      <span>{item.at ? ago(item.at) : ""}</span>
                    </button>
                  ))}
                </div>
              </section>
              <section className="thread-pane">
                {selected ? (
                  <>
                    <header className="thread-top">
                      <Avatar name={selected.name} online={online.has(selected.name)} />
                      <span className="person-line">
                        <strong>{selected.name}</strong>
                        <span>{online.has(selected.name) ? "In the room" : "Last on this computer"}</span>
                      </span>
                      <span className="thread-tools">
                        <button type="button" aria-label={`Call ${selected.name}`} onClick={() => joinRoom(selected.room)}><PhoneIcon size={18} /></button>
                        <button type="button" aria-label={`Share with ${selected.name}`} onClick={() => joinRoom(selected.room, true)}><MonitorIcon size={18} /></button>
                      </span>
                    </header>
                    <div className="transcript">
                      {thread && thread.notes.length > 0 ? <div className="day">Today</div> : null}
                      {thread?.notes.map((mail) => (
                        <article className="bubble" key={mail.id}>
                          <Avatar name={mail.author} small />
                          <div>
                            <strong>{mail.author}</strong>
                            <p>{mail.text}</p>
                            <time>{ago(mail.at)}</time>
                          </div>
                        </article>
                      ))}
                    </div>
                    {selected.room ? (
                      <div className="room-card pinned">
                        <span className="room-mark"><MonitorIcon size={18} /></span>
                        <span className="person-line">
                          <strong>Room: {selected.room}</strong>
                          <span>Voice and screen share</span>
                        </span>
                        <button className="cream" type="button" onClick={() => joinRoom(selected.room)}>Join</button>
                      </div>
                    ) : null}
                    <div className="composer">
                      <form onSubmit={(event) => { event.preventDefault(); sendNote(selected.name, selected.room); }}>
                        <input aria-label={`Message ${selected.name}`} placeholder={`Message ${selected.name}...`} value={draft} onChange={(event) => setDraft(event.target.value)} />
                        <button className="send" type="submit" aria-label="Save note"><SendIcon size={16} /></button>
                      </form>
                      <p className="hint">Notes stay on this computer. Join the room to talk.</p>
                    </div>
                  </>
                ) : (
                  <p className="empty">Pick a person, or add one, to keep a note and a room.</p>
                )}
              </section>
            </div>
          ) : null}
          {page === "recents" ? (
            <div className="page">
              <div className="page-head"><h1>Recent rooms</h1></div>
              <div className="room-list">
                {desk.recents.length === 0 ? <p className="empty">Rooms you open will stay here.</p> : null}
                {desk.recents.map((item) => (
                  <button className="room" type="button" key={item.room} onClick={() => joinRoom(item.room)}>
                    <span className="room-mark"><MonitorIcon size={18} /></span>
                    <span className="person-line">
                      <strong>{item.room}</strong>
                      <span>{item.people} {item.people === 1 ? "person" : "people"} · {ago(item.at)}</span>
                    </span>
                    <ArrowIcon size={18} />
                  </button>
                ))}
              </div>
            </div>
          ) : null}
          {page === "settings" ? (
            <div className="page">
              <div className="page-head"><h1>Settings</h1></div>
              <div className="settings">
                <label>
                  Display name
                  <input value={name} autoComplete="name" onChange={(event) => { setName(event.target.value); persist({ name: event.target.value }); }} />
                </label>
                <label>
                  Invite
                  <input value={invite} autoComplete="off" onChange={(event) => { setInvite(event.target.value); persist({ invite: event.target.value }); }} />
                </label>
                <button className="outline" type="button" onClick={() => { const next = theme === "dark" ? "light" : "dark"; setTheme(next); persist({ dark: next === "dark" }); }}>
                  {theme === "dark" ? "Switch to light" : "Switch to dark"}
                </button>
                <DeviceSelect label="Microphone" value={inputId} devices={inputs} onChange={pickInput} />
                <DeviceSelect label="Speakers" value={outputId} devices={outputs} onChange={pickOutput} />
                <NoiseSwitch on={noise} onChange={pickNoise} />
              </div>
            </div>
          ) : null}
        </main>
      </div>
      {dialog === "create" || dialog === "join" ? (
        <div className="picker" role="dialog" aria-modal="true" aria-label={dialog === "create" ? "Create a room" : "Join a room"}>
          <form className="picker-card" onSubmit={(event) => { event.preventDefault(); joinRoom(room); }}>
            <h2>{dialog === "create" ? "Create a room" : "Join a room"}</h2>
            <label>Display name<input value={name} autoComplete="name" onChange={(event) => setName(event.target.value)} /></label>
            <label>Room<input value={room} autoComplete="off" onChange={(event) => setRoom(event.target.value)} /></label>
            <label>Invite<input value={invite} autoComplete="off" placeholder="Optional" onChange={(event) => setInvite(event.target.value)} /></label>
            <div className="dialog-actions">
              <button className="outline" type="button" onClick={() => setDialog(null)}>Cancel</button>
              <button className="cream" type="submit" disabled={busy || phase === "joining"}>{phase === "joining" ? "Joining" : dialog === "create" ? "Create" : "Join"}</button>
            </div>
          </form>
        </div>
      ) : null}
      {dialog === "friend" ? (
        <div className="picker" role="dialog" aria-modal="true" aria-label="Add a friend">
          <form className="picker-card" onSubmit={(event) => { event.preventDefault(); addFriend(); }}>
            <h2>Add a friend</h2>
            <label>Name<input value={friendName} onChange={(event) => setFriendName(event.target.value)} /></label>
            <label>Room you share<input value={friendRoom} onChange={(event) => setFriendRoom(event.target.value)} /></label>
            <div className="dialog-actions">
              <button className="outline" type="button" onClick={() => setDialog(null)}>Cancel</button>
              <button className="cream" type="submit">Save</button>
            </div>
          </form>
        </div>
      ) : null}
    </div>
  );
}

function NoiseSwitch({ on, onChange }: { on: boolean; onChange: (on: boolean) => void }) {
  return (
    <button className={`switch${on ? " on" : ""}`} type="button" role="switch" aria-checked={on} onClick={() => onChange(!on)}>
      <span>Noise suppression</span>
      <i />
    </button>
  );
}

function DeviceSelect({ label, value, devices, onChange }: { label: string; value: string; devices: Gear[]; onChange: (id: string) => void }) {
  const known = devices.some((device) => device.id === value);
  return (
    <label>
      {label}
      <select aria-label={label} value={known ? value : ""} onChange={(event) => onChange(event.target.value)}>
        <option value="">System default</option>
        {devices.map((device) => (
          <option key={device.id} value={device.id}>{device.name}</option>
        ))}
      </select>
    </label>
  );
}

function conversationList(desk: Desk, query: string) {
  const needle = query.trim().toLowerCase();
  const map = new Map<string, { name: string; room: string; preview: string; at: number }>();
  for (const friend of desk.friends) {
    map.set(friend.name, { name: friend.name, room: friend.room, preview: "", at: friend.at });
  }
  for (const thread of desk.threads) {
    const last = thread.notes[thread.notes.length - 1];
    const current = map.get(thread.name);
    map.set(thread.name, {
      name: thread.name,
      room: thread.room || current?.room || "",
      preview: last?.text ?? "",
      at: last?.at ?? current?.at ?? 0
    });
  }
  return [...map.values()]
    .filter((item) => item.name.toLowerCase().includes(needle) || item.preview.toLowerCase().includes(needle))
    .sort((a, b) => b.at - a.at);
}

function Avatar({ name, online = false, large = false, small = false }: { name: string; online?: boolean; large?: boolean; small?: boolean }) {
  return (
    <span className={`avatar${large ? " lg" : ""}${small ? " sm" : ""}`} style={{ background: faceColor(name) }}>
      {initials(name)}
      {online ? <i className="presence on" /> : null}
    </span>
  );
}

function FriendTabs({ tab, setTab, online }: { tab: FriendTab; setTab: (tab: FriendTab) => void; online: number }) {
  return (
    <div className="tabs" role="tablist">
      <button type="button" className={tab === "online" ? "on" : ""} onClick={() => setTab("online")}>Online {online}</button>
      <button type="button" className={tab === "all" ? "on" : ""} onClick={() => setTab("all")}>All</button>
      <button type="button" className={tab === "requests" ? "on" : ""} onClick={() => setTab("requests")}>Requests</button>
    </div>
  );
}

function FriendRows({ friends, tab, online, onCall, onShare }: { friends: Friend[]; tab: FriendTab; online: Set<string>; onCall: (friend: Friend) => void; onShare: (friend: Friend) => void }) {
  if (tab === "requests") return <p className="empty">No requests. Add someone you already share a room with.</p>;
  if (friends.length === 0) return <p className="empty">{tab === "online" ? "No one from your list is in a room with you." : "Add someone, then call the room you both use."}</p>;
  return friends.map((friend) => (
    <div className="friend" key={friend.name}>
      <Avatar name={friend.name} online={online.has(friend.name)} />
      <span className="friend-copy">
        <strong>{friend.name}</strong>
        <span>{online.has(friend.name) ? "In the room" : `${friend.room} · ${ago(friend.at)}`}</span>
      </span>
      <button className="mini-action" type="button" aria-label={`Share with ${friend.name}`} onClick={() => onShare(friend)}><MonitorIcon size={18} /></button>
      <button className="mini-action" type="button" aria-label={`Call ${friend.name}`} onClick={() => onCall(friend)}><PhoneIcon size={18} /></button>
    </div>
  ));
}
