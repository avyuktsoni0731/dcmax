import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Note =
  | { kind: "status"; text: string }
  | { kind: "joined" }
  | { kind: "left" }
  | { kind: "sharing"; on: boolean };

const RESOLUTIONS = [
  { width: 1280, height: 720, label: "1280 × 720" },
  { width: 1920, height: 1080, label: "1920 × 1080" },
  { width: 2560, height: 1440, label: "2560 × 1440" }
];

const BITRATES = [8000, 12000, 20000, 40000, 80000, 100000];

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
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  useEffect(() => {
    const unlisten = listen<Note>("engine", (event) => {
      const note = event.payload;
      if (note.kind === "status") {
        setStatus(note.text);
        setPhase((current) => (current === "joining" ? "lobby" : current));
        setBusy(false);
      } else if (note.kind === "joined") {
        setPhase("call");
        setStatus("In the room.");
        setBusy(false);
      } else if (note.kind === "left") {
        setPhase("lobby");
        setSharing(false);
        setMuted(false);
        setStatus("You left the room.");
        setBusy(false);
      } else if (note.kind === "sharing") {
        setSharing(note.on);
        setStatus(note.on ? "Sharing your screen." : "In the room.");
      }
    });
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, []);

  useEffect(() => {
    if (phase !== "call") return;
    const node = document.getElementById("stage");
    if (!node) return;
    const post = () => {
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
  }, [phase]);

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
    return { width: size.width, height: size.height, fps, bitrateKbps: bitrate };
  };

  return (
    <div className="app">
      <header className="bar">
        <div className="brand">
          <span className="mark" aria-hidden="true" />
          MyCord
        </div>
        <div className="status">
          {phase === "joining" ? <span className="joining" /> : phase === "call" ? <span className="dot" /> : null}
          <span>{status}</span>
          <button className="ghost" type="button" onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>
            {theme === "dark" ? "Light" : "Dark"}
          </button>
        </div>
      </header>
      {phase === "call" ? (
        <main className="call">
          <div className="stage" id="stage">
            <div className="stage-copy">{sharing ? "Sharing your screen." : "Waiting for a screen."}</div>
          </div>
          <div className="dock">
            <label>
              Resolution
              <select
                value={resolution}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setResolution(next);
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
            </label>
            <label>
              Frame rate
              <select
                value={fps}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setFps(next);
                  void invoke("tune", { body: { ...picture(), fps: next } });
                }}
              >
                <option value={30}>30</option>
                <option value={60}>60</option>
              </select>
            </label>
            <label>
              Bitrate
              <select
                value={bitrate}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setBitrate(next);
                  void invoke("tune", { body: { ...picture(), bitrateKbps: next } });
                }}
              >
                {BITRATES.map((value) => (
                  <option key={value} value={value}>
                    {value / 1000} Mbps
                  </option>
                ))}
              </select>
            </label>
            <button type="button" onClick={() => void run(() => invoke("set_muted", { body: { muted: !muted } }).then(() => setMuted(!muted)))}>
              {muted ? "Unmute" : "Mute"}
            </button>
            <button
              className="primary"
              type="button"
              onClick={() => void run(() => (sharing ? invoke("stop_share") : invoke("share", { body: picture() })))}
            >
              {sharing ? "Stop sharing" : "Share screen"}
            </button>
            <button className="leave" type="button" disabled={busy} onClick={() => void run(() => invoke("leave"))}>
              Leave
            </button>
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
              setPhase("joining");
              setStatus("Joining the room.");
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
    </div>
  );
}
