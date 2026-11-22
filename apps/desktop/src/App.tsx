import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { DarkIcon, FullscreenExitIcon, FullscreenIcon, LeaveIcon, LightIcon, MicIcon, MicOffIcon, ShareIcon, StopIcon } from "./icons";
import { playEnter, playExit, playJoined, playLeft, playMute, playUnmute, primeAudio } from "./sounds";

type Note =
  | { kind: "status"; text: string }
  | { kind: "joined" }
  | { kind: "left" }
  | { kind: "sharing"; on: boolean }
  | { kind: "people"; people: Person[] };

type Person = {
  name: string;
  local: boolean;
  speaking: boolean;
};

const RESOLUTIONS = [
  { width: 1280, height: 720, label: "1280 × 720" },
  { width: 1920, height: 1080, label: "1920 × 1080" },
  { width: 2560, height: 1440, label: "2560 × 1440" }
];

const BITRATES = [8000, 12000, 20000, 40000, 80000, 100000];

type ScreenChoice = {
  id: number;
  label: string;
  width: number;
  height: number;
};

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
};

export function App() {
  const [theme, setTheme] = useState<"light" | "dark">("light");
  const [name, setName] = useState("");
  const [room, setRoom] = useState("");
  const [invite, setInvite] = useState("");
  const [phase, setPhase] = useState<"lobby" | "joining" | "call">("lobby");
  const [status, setStatus] = useState("Enter a name and a room.");
  const [sharing, setSharing] = useState(false);
  const [muted, setMuted] = useState(false);
  const [resolution, setResolution] = useState(1);
  const [fps, setFps] = useState(60);
  const [bitrate, setBitrate] = useState(40000);
  const [voice, setVoice] = useState(100);
  const [screen, setScreen] = useState(100);
  const [fullscreen, setFullscreen] = useState(false);
  const [chrome, setChrome] = useState(true);
  const [displayId, setDisplayId] = useState<number | null>(null);
  const [picker, setPicker] = useState<ScreenChoice[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [people, setPeople] = useState<Person[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const sharingRef = useRef(false);
  const remotesRef = useRef<Set<string> | null>(null);
  const noticeTimer = useRef(0);

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
    ...next
  });

  function persist(next?: Partial<Profile>) {
    void invoke("remember", { body: saved(next) });
  }

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  useEffect(() => {
    if (!import.meta.env.DEV || !new URLSearchParams(window.location.search).has("preview")) return;
    setPhase("call");
    setName("Avyukt");
    setPeople([
      { name: "Avyukt", local: true, speaking: false },
      { name: "GuessWho", local: false, speaking: true }
    ]);
    setStatus("In the room with GuessWho");
  }, []);

  useEffect(() => {
    void invoke<Profile>("profile")
      .then((stored) => {
        setName(stored.name);
        setRoom(stored.room);
        setInvite(stored.invite);
        setResolution(stored.resolution);
        setFps(stored.fps);
        setBitrate(stored.bitrateKbps);
        setTheme(stored.dark ? "dark" : "light");
        setVoice(stored.voice ?? 100);
        setScreen(stored.screen ?? 100);
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
        setPhase((current) => (current === "joining" ? "lobby" : current));
        setBusy(false);
      } else if (note.kind === "joined") {
        setPhase("call");
        setStatus("You're in the room.");
        setBusy(false);
        remotesRef.current = null;
        playJoined();
      } else if (note.kind === "left") {
        setPhase("lobby");
        setPeople([]);
        setSharing(false);
        sharingRef.current = false;
        remotesRef.current = null;
        setMuted(false);
        setFullscreen(false);
        setNotice(null);
        setStatus("You left the room.");
        setBusy(false);
        playLeft();
        void invoke("set_fullscreen", { body: { on: false } });
      } else if (note.kind === "sharing") {
        setSharing(note.on);
        sharingRef.current = note.on;
        setStatus(note.on ? "Sharing your screen." : "You're in the room.");
      } else if (note.kind === "people") {
        setPeople(note.people);
        const remotes = note.people.filter((person) => !person.local).map((person) => person.name);
        const previous = remotesRef.current;
        remotesRef.current = new Set(remotes);
        if (previous) {
          for (const person of remotes) {
            if (!previous.has(person)) {
              flash(`${person} joined`);
              playEnter();
            }
          }
          for (const person of previous) {
            if (!remotes.includes(person)) {
              flash(`${person} left`);
              playExit();
            }
          }
        } else if (remotes.length > 0) {
          window.setTimeout(() => playEnter(), 220);
        }
        if (!sharingRef.current) {
          setStatus(remotes.length > 0 ? `In the room with ${remotes.join(", ")}` : "You're in the room.");
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
        void invoke("stage_rect", { body: { x: 0, y: 0, width: 0, height: 0, visible: false } });
        return;
      }
      const rect = node.getBoundingClientRect();
      void invoke("stage_rect", {
        body: { x: rect.x, y: rect.y, width: rect.width, height: rect.height, visible: true }
      });
    };
    post();
    const observer = new ResizeObserver(post);
    observer.observe(node);
    window.addEventListener("resize", post);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", post);
      void invoke("stage_rect", { body: { x: 0, y: 0, width: 0, height: 0, visible: false } });
    };
  }, [phase, picker, fullscreen, chrome, people.length]);

  useEffect(() => {
    if (phase !== "call") return;
    void invoke("set_voice", { body: { percent: voice } }).catch(() => {});
    void invoke("set_screen", { body: { percent: screen } }).catch(() => {});
  }, [phase, voice, screen]);

  async function run(task: () => Promise<unknown>, settle = true) {
    setBusy(true);
    try {
      await task();
      if (settle) setBusy(false);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
      setBusy(false);
      setPhase((current) => (current === "joining" ? "lobby" : current));
    }
  }

  const picture = () => {
    const size = RESOLUTIONS[resolution] ?? RESOLUTIONS[1];
    return { width: size.width, height: size.height, fps, bitrateKbps: bitrate, displayId };
  };

  async function shareScreen() {
    if (sharing) {
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

  return (
    <div className={`app${fullscreen ? " fullscreen" : ""}${fullscreen && chrome ? " chrome" : ""}`}>
      <header className="bar">
        <div className="brand">
          <span className="mark" aria-hidden="true" />
          MyCord
        </div>
        <div className="status">
          {phase === "joining" ? <span className="joining" /> : phase === "call" ? <span className="dot" /> : null}
          <span>{notice ?? status}</span>
          <button
            className="icon ghost"
            type="button"
            aria-label={theme === "dark" ? "Switch to light" : "Switch to dark"}
            title={theme === "dark" ? "Light" : "Dark"}
            onClick={() => {
              const next = theme === "dark" ? "light" : "dark";
              setTheme(next);
              persist({ dark: next === "dark" });
            }}
          >
            {theme === "dark" ? <LightIcon /> : <DarkIcon />}
          </button>
        </div>
      </header>
      {phase === "call" ? (
        <main className="call">
          <People people={people.length > 0 ? people : [{ name: name || "You", local: true, speaking: false }]} muted={muted} />
          <div className="stage" id="stage">
            <div className="stage-copy">{sharing ? "Sharing your screen." : "Waiting for a screen."}</div>
          </div>
          <div className="dock">
            <div className="tunes">
              <select
                aria-label="Resolution"
                value={resolution}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setResolution(next);
                  persist({ resolution: next });
                  const size = RESOLUTIONS[next] ?? RESOLUTIONS[1];
                  void invoke("tune", { body: { width: size.width, height: size.height, fps, bitrateKbps: bitrate } });
                }}
              >
                {RESOLUTIONS.map((item, index) => (
                  <option key={item.label} value={index}>
                    {item.label}
                  </option>
                ))}
              </select>
              <select
                aria-label="Frame rate"
                value={fps}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setFps(next);
                  persist({ fps: next });
                  void invoke("tune", { body: { ...picture(), fps: next } });
                }}
              >
                <option value={30}>30 fps</option>
                <option value={60}>60 fps</option>
              </select>
              <select
                aria-label="Bitrate"
                value={bitrate}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setBitrate(next);
                  persist({ bitrateKbps: next });
                  void invoke("tune", { body: { ...picture(), bitrateKbps: next } });
                }}
              >
                {BITRATES.map((value) => (
                  <option key={value} value={value}>
                    {value / 1000} Mbps
                  </option>
                ))}
              </select>
            </div>
            <div className="boosts">
              <label className="boost">
                Voice
                <input
                  type="range"
                  min={0}
                  max={200}
                  step={5}
                  value={voice}
                  aria-label="Other person's volume"
                  onChange={(event) => {
                    const next = Number(event.target.value);
                    setVoice(next);
                    persist({ voice: next });
                  }}
                />
                <span>{voice}%</span>
              </label>
              <label className="boost">
                Screen
                <input
                  type="range"
                  min={0}
                  max={200}
                  step={5}
                  value={screen}
                  aria-label="Screen audio volume"
                  onChange={(event) => {
                    const next = Number(event.target.value);
                    setScreen(next);
                    persist({ screen: next });
                  }}
                />
                <span>{screen}%</span>
              </label>
            </div>
            <div className="actions">
              <button
                className={`icon${muted ? " muted" : ""}`}
                type="button"
                aria-pressed={muted}
                aria-label={muted ? "Unmute" : "Mute"}
                title={muted ? "Unmute" : "Mute"}
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
              <button
                className={`icon${sharing ? " on" : ""}`}
                type="button"
                aria-pressed={sharing}
                aria-label={sharing ? "Stop sharing" : "Share screen"}
                title={sharing ? "Stop sharing" : "Share screen"}
                onClick={() => void run(shareScreen)}
              >
                {sharing ? <StopIcon /> : <ShareIcon />}
              </button>
              <button
                className="icon"
                type="button"
                aria-pressed={fullscreen}
                aria-label={fullscreen ? "Exit full screen" : "Full screen"}
                title={fullscreen ? "Exit full screen" : "Full screen"}
                onClick={() => {
                  const on = !fullscreen;
                  setFullscreen(on);
                  void invoke("set_fullscreen", { body: { on } });
                }}
              >
                {fullscreen ? <FullscreenExitIcon /> : <FullscreenIcon />}
              </button>
              <button
                className="icon leave"
                type="button"
                disabled={busy}
                aria-label="Leave"
                title="Leave"
                onClick={() => {
                  primeAudio();
                  void run(() => invoke("leave"));
                }}
              >
                <LeaveIcon />
              </button>
            </div>
          </div>
        </main>
      ) : (
        <main className="lobby">
          <h1>Open a room</h1>
          <p className="lead">Same room name on both computers. Voice and the screen stay on this machine.</p>
          <label>
            Display name
            <input value={name} onChange={(event) => setName(event.target.value)} autoComplete="name" />
          </label>
          <label>
            Room
            <input value={room} onChange={(event) => setRoom(event.target.value)} autoComplete="off" />
          </label>
          <label>
            Invite
            <input value={invite} onChange={(event) => setInvite(event.target.value)} autoComplete="off" />
          </label>
          <button
            className="primary"
            type="button"
            disabled={busy || phase === "joining"}
            onClick={() => {
              primeAudio();
              setPhase("joining");
              setStatus("Joining the room.");
              persist();
              void run(
                () => invoke("join", { body: { name, room, invite: invite.trim() ? invite.trim() : null } }),
                false
              );
            }}
          >
            {phase === "joining" ? "Joining" : "Join"}
          </button>
        </main>
      )}
      {picker ? (
        <div className="picker" role="dialog" aria-modal="true" aria-label="Choose a screen">
          <div className="picker-card">
            <h2>Choose a screen</h2>
            {picker.map((screen) => (
              <button
                key={screen.id}
                type="button"
                onClick={() => {
                  const next = screen.id;
                  setPicker(null);
                  setDisplayId(next);
                  void run(() => invoke("share", { body: { ...picture(), displayId: next } }));
                }}
              >
                {screen.label}
              </button>
            ))}
            <button className="ghost" type="button" onClick={() => setPicker(null)}>
              Cancel
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

const FACES = ["#e4d3b4", "#d5ddd4", "#e3d4cc", "#d3dbe3", "#ddd6c8", "#e7d5c8"];

function faceColor(name: string) {
  let hash = 0;
  for (const char of name) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return FACES[hash % FACES.length];
}

function initials(name: string) {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return "?";
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
  return `${parts[0][0]}${parts[1][0]}`.toUpperCase();
}

function People({ people, muted }: { people: Person[]; muted: boolean }) {
  return (
    <div className="people" aria-label="People in the room">
      {people.map((person, index) => {
        const talking = person.speaking && !(person.local && muted);
        return (
          <div className={`person${talking ? " speaking" : ""}`} key={`${person.local ? "you" : "them"}-${person.name}-${index}`}>
            <div className="avatar" style={{ background: faceColor(person.name) }}>
              {initials(person.name)}
            </div>
            <div className="person-name" title={person.name}>
              {person.name}
            </div>
            <div className="person-meta">{person.local ? "You" : talking ? "Speaking" : "\u00a0"}</div>
          </div>
        );
      })}
    </div>
  );
}
