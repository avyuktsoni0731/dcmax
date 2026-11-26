import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import ramenUrl from "./ramen.svg";
import { ago, faceColor, initials, loadDesk, saveDesk, type Desk, type Friend, type Mail } from "./desk";
import {
  AddIcon,
  ArrowIcon,
  ChevronIcon,
  FriendsIcon,
  FullscreenExitIcon,
  FullscreenIcon,
  HeadphonesIcon,
  HomeIcon,
  MessagesIcon,
  MicIcon,
  MicOffIcon,
  MonitorIcon,
  PhoneIcon,
  RecentsIcon,
  SearchIcon,
  SendIcon,
  SettingsIcon,
  ShareIcon
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
type Menu = "share" | "audio" | "hear" | "devices" | "stream" | "resolution" | "fps" | "bitrate" | "screens" | null;
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
    if (preview !== "inbox") setActive("souvlakee");
    if (preview === "call" || preview === "share" || preview === "options" || preview === "mics" || preview === "screens") {
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
      setStatus("In the room");
    } else if (preview === "inbox") {
      setPage("messages");
      setActive("");
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
  }, [phase, fullscreen, chrome]);

  useEffect(() => {
    if (phase !== "call") return;
    void invoke("set_voice", { body: { percent: voice } }).catch(() => {});
    void invoke("set_screen", { body: { percent: screen } }).catch(() => {});
  }, [phase, voice, screen]);

  useEffect(() => {
    if (!menu) return;
    const close = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Element && target.closest(".tool, .strip, .flyout")) return;
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
    const size = RESOLUTIONS[resolution] ?? RESOLUTIONS[1];
    const volumeOpen = menu === "audio" || menu === "hear";
    return (
      <div className={`app mock${fullscreen ? " fullscreen" : ""}${fullscreen && chrome ? " chrome" : ""}`}>
        <div className="shell call-shell">
          <Rail page={null} onPage={() => {}} />
          <div className="workspace call">
            <header className="topbar">
              <span className="topbar-title"><PhoneIcon size={18} /> Call</span>
              <span className={`call-live${status && /microphone|could not|did not/i.test(status) ? " warn" : ""}`}>{notice ?? status}</span>
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
            <div
              className="call-panel"
              onContextMenu={(event) => {
                event.preventDefault();
                setMenu(sharing ? "hear" : "audio");
              }}
            >
              <div className="stage" id="stage">
                <div className="stage-copy">{sharing ? "Sharing your screen." : "Waiting for a screen."}</div>
              </div>
              <aside className="call-side" onContextMenu={(event) => event.stopPropagation()}>
                {volumeOpen ? (
                  <div className="flyout" role="dialog" aria-label={menu === "hear" ? "Screen audio" : "Voice"}>
                    <p>Right Click Anywhere</p>
                    {menu === "hear" ? (
                      <VolumeRow label="Audio" value={screen} onChange={changeScreen} />
                    ) : (
                      <VolumeRow label="Voice" value={voice} onChange={changeVoice} />
                    )}
                    {menu === "hear" ? (
                      <VolumeRow label="Voice" value={voice} onChange={changeVoice} />
                    ) : (
                      <VolumeRow label="Audio" value={screen} onChange={changeScreen} />
                    )}
                  </div>
                ) : null}
                {menu === "devices" ? (
                  <div className="flyout devices" role="dialog" aria-label="Microphone settings">
                    <DeviceSelect label="Input Device" value={inputId} devices={inputs} onChange={pickInput} />
                    <DeviceSelect label="Output Device" value={outputId} devices={outputs} onChange={pickOutput} />
                    <NoiseSwitch on={noise} onChange={pickNoise} />
                  </div>
                ) : null}
                {menu === "share" ? (
                  <div className="flyout menu-card" role="dialog" aria-label="Share">
                    <button type="button" onClick={() => void showScreens()}>Select Screen <ChevronIcon size={16} /></button>
                    <button type="button" onClick={() => setMenu("stream")}>Stream Options <ChevronIcon size={16} /></button>
                  </div>
                ) : null}
                {menu === "screens" ? (
                  <div className="flyout menu-card" role="dialog" aria-label="Select a screen">
                    {picker && picker.length > 0 ? picker.map((choice) => (
                      <button key={choice.id} type="button" onClick={() => void run(() => useScreen(choice.id))}>
                        {choice.label} {choice.width}x{choice.height}
                      </button>
                    )) : <p>No screens found.</p>}
                  </div>
                ) : null}
                {menu === "stream" ? (
                  <div className="flyout menu-card" role="dialog" aria-label="Stream options">
                    <button type="button" onClick={() => setMenu("resolution")}>Resolution <span>{size.label.replace(" × ", "x")}</span></button>
                    <button type="button" onClick={() => setMenu("fps")}>Framerate <span>{fps} Hz</span></button>
                    <button type="button" onClick={() => setMenu("bitrate")}>Bitrate <span>{bitrate / 1000} Mbps</span></button>
                  </div>
                ) : null}
                {menu === "resolution" ? (
                  <div className="flyout menu-card" role="dialog" aria-label="Resolution">
                    {[...RESOLUTIONS].reverse().map((item) => {
                      const index = RESOLUTIONS.indexOf(item);
                      return (
                        <button key={item.label} type="button" className={index === resolution ? "picked" : ""} onClick={() => tune({ resolution: index })}>
                          <i /> {item.label.replace(" × ", "x")}
                        </button>
                      );
                    })}
                  </div>
                ) : null}
                {menu === "fps" ? (
                  <div className="flyout menu-card" role="dialog" aria-label="Frame rate">
                    {[30, 60].map((value) => (
                      <button key={value} type="button" className={fps === value ? "picked" : ""} onClick={() => tune({ fps: value })}>
                        <i /> {value} Hz
                      </button>
                    ))}
                  </div>
                ) : null}
                {menu === "bitrate" ? (
                  <div className="flyout menu-card" role="dialog" aria-label="Bitrate">
                    {BITRATES.map((value) => (
                      <button key={value} type="button" className={bitrate === value ? "picked" : ""} onClick={() => tune({ bitrate: value })}>
                        <i /> {value / 1000} Mbps
                      </button>
                    ))}
                  </div>
                ) : null}
                <div className="tile-stack">
                  {sharing && featured ? (
                    <div className={`face-tile${talking ? " speaking" : ""}`}>
                      <Avatar name={featured.name} large />
                    </div>
                  ) : null}
                  <div className={`face-tile you-tile${you?.speaking && !muted ? " speaking" : ""}`}>
                    <Avatar name={you?.name || name || "You"} large />
                    <span>{you?.name || name || "You"}</span>
                  </div>
                </div>
              </aside>
              <div className="call-dock">
                <div className={`name-pill${talking ? " speaking" : ""}`}>
                  <strong>{featured?.name || "Just you"}</strong>
                </div>
                <div className="controls">
                  <div className={`tool${muted ? " muted" : ""}`}>
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
                    <button className="caret" type="button" aria-label="Microphone settings" onClick={() => setMenu(menu === "devices" ? null : "devices")}>
                      <ChevronIcon size={16} />
                    </button>
                  </div>
                  <div className={`tool${deaf ? " muted" : ""}`}>
                    <button type="button" aria-pressed={deaf} aria-label={deaf ? "Hear the room" : "Silence the room"} onClick={toggleDeaf}>
                      <HeadphonesIcon />
                    </button>
                    <button className="caret" type="button" aria-label="Volume" onClick={() => setMenu(volumeOpen ? null : sharing ? "hear" : "audio")}>
                      <ChevronIcon size={16} />
                    </button>
                  </div>
                  <span className="control-split" />
                  <div className={`tool${sharing ? " on" : ""}`}>
                    <button type="button" aria-pressed={sharing} aria-label={sharing ? "Stop sharing" : "Share screen"} onClick={() => void run(shareScreen)}>
                      <ShareIcon size={18} />
                    </button>
                    <button className="caret" type="button" aria-label="Share settings" onClick={() => setMenu(menu === "share" ? null : "share")}>
                      <ChevronIcon size={16} />
                    </button>
                  </div>
                  <button className="hangup" type="button" disabled={busy} aria-label="Leave" onClick={() => { primeAudio(); void run(() => invoke("leave")); }}>
                    <PhoneIcon size={20} />
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
    );
  }

  const threadNames = conversationList(desk, query);
  const selectedName = threadNames.some((item) => item.name === active) ? active : null;
  const selected = threadNames.find((item) => item.name === selectedName) ?? null;
  const thread = desk.threads.find((item) => item.name === selectedName);

  return (
    <div className={`app${page === "messages" ? " mock" : ""}`}>
      <div className="shell">
        <Rail page={page} onPage={setPage} />
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
              <header className="topbar">
                <span className="topbar-title"><FriendsIcon size={18} /> Friends</span>
              </header>
              <div className="chat-board">
                <section className="dm-col">
                  <label className="search">
                    <SearchIcon size={16} />
                    <input id="thread-search" placeholder="Find your friend" value={query} onChange={(event) => setQuery(event.target.value)} />
                  </label>
                  <div className="section-head">
                    <h2>Direct Messages</h2>
                    <button className="icon-quiet" type="button" aria-label="Add a friend" onClick={() => setDialog("friend")}><AddIcon size={16} /></button>
                  </div>
                  <div className="thread-scroll">
                    {threadNames.length === 0 ? <p className="empty">Add a friend, then open a room together.</p> : null}
                    {threadNames.map((item) => {
                      const count = desk.threads.find((threadItem) => threadItem.name === item.name)?.notes.length ?? 0;
                      const badge = item.name === selectedName || count === 0 ? "" : count > 9 ? "9+" : String(count);
                      return (
                        <button className={`thread-row${item.name === selectedName ? " active" : ""}`} type="button" key={item.name} onClick={() => setActive(item.name)}>
                          <Avatar name={item.name} online={online.has(item.name)} />
                          <span className="thread-copy">
                            <strong>{item.name}</strong>
                          </span>
                          {badge ? <span className="badge">{badge}</span> : null}
                        </button>
                      );
                    })}
                  </div>
                  <div className="you dm-you">
                    <Avatar name={name || "You"} online />
                    <span className="person-line">
                      <strong>{name || "Your name"}</strong>
                      <span>Online</span>
                    </span>
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
                          <button type="button" aria-label={`Call ${selected.name}`} onClick={() => joinRoom(selected.room)}><PhoneIcon size={18} /></button>
                          <button type="button" aria-label={`Share with ${selected.name}`} onClick={() => joinRoom(selected.room, true)}><MonitorIcon size={18} /></button>
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
                          <button className="emoji" type="button" aria-label="Add an emoji" onClick={() => setDraft((current) => `${current}✨`)}>☺</button>
                          <button className="send" type="submit" aria-label="Save note"><SendIcon size={16} /></button>
                        </form>
                        <p className="hint">Notes stay on this computer. The phone button joins the room.</p>
                      </div>
                    </>
                  ) : (
                    <div className="chat-empty">
                      <img src={ramenUrl} alt="" />
                      <p>Ramen may have originated from China. Scholars believe that “shina shoba” (“Chinese style” noodle soup) was imported to Japan between the 17th and 20th centuries.</p>
                    </div>
                  )}
                </section>
              </div>
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

function Rail({ page, onPage }: { page: Page | null; onPage: (page: Page) => void }) {
  const main = NAV.filter((item) => item.id !== "settings");
  return (
    <aside className="sidebar">
      <button className="brand-mark" type="button" aria-label="Home" onClick={() => onPage("home")}>M</button>
      <nav className="nav-list" aria-label="Sections">
        {main.map((item) => (
          <button key={item.id} className={`nav-btn${page === item.id ? " active" : ""}`} type="button" aria-label={item.label} onClick={() => onPage(item.id)}>
            <item.icon size={22} />
          </button>
        ))}
      </nav>
      <button className={`nav-btn rail-end${page === "settings" ? " active" : ""}`} type="button" aria-label="Settings" onClick={() => onPage("settings")}>
        <SettingsIcon size={22} />
      </button>
    </aside>
  );
}

function VolumeRow({ label, value, onChange }: { label: string; value: number; onChange: (value: number) => void }) {
  return (
    <label className="volume-row">
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

function clock(at: number) {
  return new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }).replace(" AM", " am").replace(" PM", " pm");
}

function dayTitle(at: number) {
  const date = new Date(at);
  const months = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
  return `${months[date.getMonth()]} ${date.getDate()}, ${date.getFullYear()}`;
}

function noteDays(notes: Mail[]) {
  const groups: { day: string; notes: Mail[] }[] = [];
  for (const mail of notes) {
    const day = dayTitle(mail.at);
    const last = groups[groups.length - 1];
    if (last && last.day === day) last.notes.push(mail);
    else groups.push({ day, notes: [mail] });
  }
  return groups;
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
