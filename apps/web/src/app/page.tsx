"use client";

import {
  ConnectionQuality,
  ConnectionState,
  LocalParticipant,
  Participant,
  ParticipantEvent,
  Room,
  RoomEvent,
  Track,
  VideoPresets
} from "livekit-client";
import { useEffect, useRef, useState } from "react";
import {
  BrowserName,
  OsName,
  detectBrowser,
  detectOs,
  getSystemAudioSupportMessage
} from "@/lib/capabilities";
import { QUALITY_PROFILES, QualityMode, resolveCodecPreference } from "@/lib/quality";

type CallState = "idle" | "connecting" | "connected" | "reconnecting" | "ended";

type TokenResponse = {
  token: string;
  url: string;
};
type NativeSession = {
  roomName: string;
  identity: string;
  backend: string;
  achievedFps: number;
  producedFrames: number;
  droppedFrames: number;
  avgIngestLatencyMs: number;
  avgPayloadBytes: number;
  updatedAt: string;
};
type NativeSessionResponse = {
  roomName: string;
  count: number;
  sessions: NativeSession[];
};

type ToastTone = "neutral" | "success" | "warning" | "error";
type UaInfo = {
  browser: BrowserName;
  os: OsName;
  message: string;
};
type ScreenCaptureStats = {
  width: number;
  height: number;
  frameRate: number;
};

const API_BASE = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:4000";

function classNames(...parts: Array<string | false | null | undefined>) {
  return parts.filter(Boolean).join(" ");
}

function qualityText(q: ConnectionQuality): "good" | "fair" | "poor" {
  if (q === ConnectionQuality.Excellent) return "good";
  if (q === ConnectionQuality.Good) return "fair";
  return "poor";
}

function formatElapsed(ms: number): string {
  const total = Math.floor(ms / 1000);
  const hh = Math.floor(total / 3600);
  const mm = Math.floor((total % 3600) / 60);
  const ss = total % 60;
  if (hh > 0) return `${String(hh).padStart(2, "0")}:${String(mm).padStart(2, "0")}:${String(ss).padStart(2, "0")}`;
  return `${String(mm).padStart(2, "0")}:${String(ss).padStart(2, "0")}`;
}

export default function HomePage() {
  const [username, setUsername] = useState("");
  const [roomName, setRoomName] = useState("mycord-room");
  const [callState, setCallState] = useState<CallState>("idle");
  const [room, setRoom] = useState<Room | null>(null);
  const [isMuted, setIsMuted] = useState(false);
  const [isSharingScreen, setIsSharingScreen] = useState(false);
  const [qualityMode, setQualityMode] = useState<QualityMode>("balanced");
  const [audioInputs, setAudioInputs] = useState<MediaDeviceInfo[]>([]);
  const [selectedMicId, setSelectedMicId] = useState<string>("");
  const [remoteIdentity, setRemoteIdentity] = useState("Waiting for participant");
  const [remoteIsSpeaking, setRemoteIsSpeaking] = useState(false);
  const [localIsSpeaking, setLocalIsSpeaking] = useState(false);
  const [connectionPill, setConnectionPill] = useState<"good" | "fair" | "poor">("good");
  const [toastText, setToastText] = useState("");
  const [toastTone, setToastTone] = useState<ToastTone>("neutral");
  const [connectedAt, setConnectedAt] = useState<number | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [isRemoteFullscreen, setIsRemoteFullscreen] = useState(false);
  const [copiedInvite, setCopiedInvite] = useState(false);
  const [screenCaptureStats, setScreenCaptureStats] = useState<ScreenCaptureStats | null>(null);
  const [nativeSession, setNativeSession] = useState<NativeSession | null>(null);
  const [uaInfo, setUaInfo] = useState<UaInfo>({
    browser: "other",
    os: "other",
    message: "Checking browser capabilities..."
  });

  const remoteVideoRef = useRef<HTMLVideoElement | null>(null);
  const localVideoRef = useRef<HTMLVideoElement | null>(null);
  const remoteAudioContainerRef = useRef<HTMLDivElement | null>(null);

  function showToast(text: string, tone: ToastTone = "neutral") {
    setToastText(text);
    setToastTone(tone);
  }

  useEffect(() => {
    const browser = detectBrowser(window.navigator.userAgent);
    const os = detectOs(window.navigator.userAgent);
    setUaInfo({
      browser,
      os,
      message: getSystemAudioSupportMessage(browser, os)
    });
  }, []);

  useEffect(() => {
    let mounted = true;
    const mediaDevices = window.navigator?.mediaDevices;
    if (!mediaDevices || typeof mediaDevices.enumerateDevices !== "function") {
      setAudioInputs([]);
      const currentOrigin = window.location.origin;
      const secureHint = window.isSecureContext
        ? ""
        : ` Current origin is ${currentOrigin}; use HTTPS (or localhost) to enable media APIs.`;
      showToast(`This browser/context does not expose media devices.${secureHint}`, "warning");
      return () => {
        mounted = false;
      };
    }

    mediaDevices
      .enumerateDevices()
      .then((devices) => {
        if (!mounted) return;
        const mics = devices.filter((d) => d.kind === "audioinput");
        setAudioInputs(mics);
        if (mics[0]) setSelectedMicId(mics[0].deviceId);
      })
      .catch(() => {
        setAudioInputs([]);
        showToast("Unable to read media devices. Check site permissions for microphone access.", "warning");
      });

    return () => {
      mounted = false;
    };
  }, []);

  useEffect(() => {
    return () => {
      room?.disconnect();
    };
  }, [room]);

  useEffect(() => {
    if (!connectedAt) {
      setElapsedMs(0);
      return;
    }
    const timer = window.setInterval(() => {
      setElapsedMs(Date.now() - connectedAt);
    }, 1000);
    return () => window.clearInterval(timer);
  }, [connectedAt]);

  useEffect(() => {
    const onKeyDown = (ev: KeyboardEvent) => {
      if (!room) return;
      if (ev.target instanceof HTMLInputElement || ev.target instanceof HTMLSelectElement) return;
      if (ev.key.toLowerCase() === "m") {
        ev.preventDefault();
        void toggleMute();
      }
      if (ev.key.toLowerCase() === "s") {
        ev.preventDefault();
        void toggleScreenShare();
      }
      if (ev.key.toLowerCase() === "f") {
        ev.preventDefault();
        void toggleRemoteFullscreen();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  useEffect(() => {
    if (!toastText) return;
    const t = window.setTimeout(() => setToastText(""), 5000);
    return () => window.clearTimeout(t);
  }, [toastText]);

  useEffect(() => {
    const roomKey = roomName.trim().replace(/\s+/g, "_");
    if (!roomKey) {
      setNativeSession(null);
      return;
    }

    let active = true;
    const poll = async () => {
      try {
        const res = await fetch(`${API_BASE}/native/sessions/${roomKey}`);
        if (!res.ok) return;
        const payload = (await res.json()) as NativeSessionResponse;
        if (!active) return;
        const preferred =
          payload.sessions.find((s) => s.identity.toLowerCase().includes("native-sender")) ??
          payload.sessions[0] ??
          null;
        setNativeSession(preferred);
      } catch {
        // best-effort polling, no user-facing toast needed
      }
    };

    void poll();
    const id = window.setInterval(() => {
      void poll();
    }, Boolean(room) ? 4000 : 10000);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, [roomName, room]);

  async function requestToken(identity: string, roomToJoin: string): Promise<TokenResponse> {
    const res = await fetch(`${API_BASE}/token`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ identity, roomName: roomToJoin })
    });

    if (!res.ok) {
      throw new Error("Token request failed");
    }
    return (await res.json()) as TokenResponse;
  }

  function attachRemoteTrack(participant: Participant) {
    const publications = Array.from(participant.trackPublications.values());
    const screenPub = publications.find(
      (p) => p.track?.kind === Track.Kind.Video && p.trackName.includes("screen")
    );
    const camPub = publications.find((p) => p.track?.kind === Track.Kind.Video);
    const targetPub = screenPub ?? camPub;
    const track = targetPub?.videoTrack;
    if (track && remoteVideoRef.current) {
      track.attach(remoteVideoRef.current);
    }
  }

  function pickPreferredParticipant(roomInstance: Room): Participant | null {
    const participants = Array.from(roomInstance.remoteParticipants.values());
    if (participants.length === 0) return null;
    const native = participants.find((p) => p.identity.toLowerCase().includes("native"));
    return native ?? participants[0];
  }

  function refreshPreferredRemote(roomInstance: Room) {
    const preferred = pickPreferredParticipant(roomInstance);
    if (!preferred) {
      setRemoteIdentity("Waiting for participant");
      setRemoteIsSpeaking(false);
      if (remoteVideoRef.current) remoteVideoRef.current.srcObject = null;
      if (remoteAudioContainerRef.current) remoteAudioContainerRef.current.innerHTML = "";
      return;
    }
    setRemoteIdentity(preferred.identity);
    setRemoteIsSpeaking(preferred.isSpeaking);
    attachRemoteTrack(preferred);
    attachRemoteAudio(preferred);
  }

  function attachRemoteAudio(participant: Participant) {
    if (!remoteAudioContainerRef.current) return;
    remoteAudioContainerRef.current.innerHTML = "";

    const audioPublications = Array.from(participant.trackPublications.values()).filter(
      (pub) => pub.track?.kind === Track.Kind.Audio
    );

    for (const pub of audioPublications) {
      const audioTrack = pub.audioTrack;
      if (!audioTrack) continue;
      const el = audioTrack.attach();
      el.autoplay = true;
      el.controls = false;
      el.className = "hidden";
      remoteAudioContainerRef.current.appendChild(el);
      void el.play().catch(() => {
        showToast("Remote audio is blocked by autoplay policy. Click anywhere and try again.", "warning");
      });
    }
  }

  function attachLocalScreen(roomInstance: Room) {
    const screenPub = Array.from(roomInstance.localParticipant.videoTrackPublications.values()).find((p) =>
      p.trackName.includes("screen")
    );
    if (screenPub?.videoTrack && localVideoRef.current) {
      screenPub.videoTrack.attach(localVideoRef.current);
      const settings = screenPub.videoTrack.mediaStreamTrack.getSettings();
      setScreenCaptureStats({
        width: settings.width ?? 0,
        height: settings.height ?? 0,
        frameRate: Math.round(settings.frameRate ?? 0)
      });
    } else if (localVideoRef.current) {
      localVideoRef.current.srcObject = null;
      setScreenCaptureStats(null);
    }
  }

  async function joinCall() {
    setToastText("");
    setCallState("connecting");
    try {
      if (!window.navigator?.mediaDevices) {
        const secureHint = window.isSecureContext
          ? ""
          : ` Current origin is ${window.location.origin}; use HTTPS (or localhost).`;
        throw new Error(
          `Media APIs are unavailable in this browser/context. Try Chrome/Edge with HTTPS.${secureHint}`
        );
      }

      const identity = username.trim().replace(/\s+/g, "_");
      if (!identity) throw new Error("Please enter a username");
      const sanitizedRoom = roomName.trim().replace(/\s+/g, "_");
      if (!sanitizedRoom) throw new Error("Please enter a room name");

      const tokenData = await requestToken(identity, sanitizedRoom);
      const codecOrder = resolveCodecPreference();
      const quality = QUALITY_PROFILES[qualityMode];

      const roomInstance = new Room({
        adaptiveStream: true,
        dynacast: true,
        publishDefaults: {
          videoSimulcastLayers: [VideoPresets.h180, VideoPresets.h360, VideoPresets.h720],
          videoCodec: codecOrder[0],
          screenShareEncoding: {
            maxBitrate: quality.maxBitrate,
            maxFramerate: quality.frameRate
          }
        }
      });

      roomInstance.on(RoomEvent.ConnectionStateChanged, (state) => {
        if (state === ConnectionState.Connected) {
          setCallState("connected");
          setConnectedAt(Date.now());
        }
        if (state === ConnectionState.Disconnected) {
          setCallState("ended");
          setConnectedAt(null);
        }
      });
      roomInstance.on(RoomEvent.Reconnecting, () => setCallState("reconnecting"));
      roomInstance.on(RoomEvent.Reconnected, () => setCallState("connected"));
      roomInstance.on(RoomEvent.ParticipantConnected, (participant) => {
        setRemoteIsSpeaking(participant.isSpeaking);
        refreshPreferredRemote(roomInstance);
      });
      roomInstance.on(RoomEvent.ParticipantDisconnected, () => {
        refreshPreferredRemote(roomInstance);
      });
      roomInstance.on(RoomEvent.TrackSubscribed, () => {
        refreshPreferredRemote(roomInstance);
      });
      roomInstance.on(RoomEvent.LocalTrackPublished, () => {
        attachLocalScreen(roomInstance);
      });
      roomInstance.on(RoomEvent.ConnectionQualityChanged, (qualityValue, participant) => {
        if (participant.identity === roomInstance.localParticipant.identity) {
          setConnectionPill(qualityText(qualityValue));
        }
      });

      await roomInstance.connect(tokenData.url, tokenData.token, { autoSubscribe: true });

      if (selectedMicId) {
        await roomInstance.switchActiveDevice("audioinput", selectedMicId);
      }
      await roomInstance.localParticipant.setMicrophoneEnabled(true, {
        echoCancellation: true,
        noiseSuppression: true
      });

      const localParticipant = roomInstance.localParticipant;
      setLocalIsSpeaking(localParticipant.isSpeaking);
      localParticipant.on(ParticipantEvent.IsSpeakingChanged, () => {
        setLocalIsSpeaking(localParticipant.isSpeaking);
      });

      setRoom(roomInstance);
      setCallState("connected");
      setConnectedAt(Date.now());
      showToast("Joined room successfully.", "success");
    } catch (err) {
      setCallState("idle");
      showToast(err instanceof Error ? err.message : "Failed to join call", "error");
    }
  }

  async function leaveCall() {
    if (!room) return;
    room.disconnect();
    setRoom(null);
    setCallState("ended");
    setConnectedAt(null);
    setIsSharingScreen(false);
    setIsRemoteFullscreen(false);
    setRemoteIsSpeaking(false);
    setRemoteIdentity("Waiting for participant");
    if (remoteVideoRef.current) remoteVideoRef.current.srcObject = null;
    if (localVideoRef.current) localVideoRef.current.srcObject = null;
    showToast("Call ended.", "neutral");
  }

  async function toggleMute() {
    if (!room) return;
    const local = room.localParticipant as LocalParticipant;
    await local.setMicrophoneEnabled(isMuted);
    setIsMuted((v) => !v);
    showToast(isMuted ? "Microphone enabled." : "Microphone muted.", "neutral");
  }

  async function switchMic(micId: string) {
    setSelectedMicId(micId);
    if (!room) return;
    await room.switchActiveDevice("audioinput", micId);
  }

  async function toggleScreenShare() {
    if (!room) return;
    if (!window.navigator?.mediaDevices?.getDisplayMedia) {
      showToast("Screen sharing is not available in this browser.", "warning");
      return;
    }

    const quality = QUALITY_PROFILES[qualityMode];
    try {
      if (isSharingScreen) {
        await room.localParticipant.setScreenShareEnabled(false);
        setIsSharingScreen(false);
        if (localVideoRef.current) localVideoRef.current.srcObject = null;
        setScreenCaptureStats(null);
        return;
      }

      const localParticipant = room.localParticipant;
      const publishOptions = {
        screenShareEncoding: {
          maxBitrate: quality.maxBitrate,
          maxFramerate: quality.frameRate
        }
      };
      const audioConstraints: MediaTrackConstraints = {
        // Keep shared-system audio raw to preserve source volume dynamics.
        echoCancellation: false,
        noiseSuppression: false,
        autoGainControl: false
      };

      await localParticipant.setScreenShareEnabled(
        true,
        {
          audio: audioConstraints as unknown as DisplayMediaStreamOptions["audio"],
          video: true,
          resolution: {
            width: quality.width,
            height: quality.height,
            frameRate: quality.frameRate
          },
          contentHint: "motion",
          selfBrowserSurface: "include",
          systemAudio: "include"
        },
        publishOptions
      );
      setIsSharingScreen(true);
      attachLocalScreen(room);
      showToast(
        `Screen share started. Target ${quality.width}x${quality.height} @ ${quality.frameRate}fps.`,
        "success"
      );
    } catch (err) {
      showToast(
        err instanceof Error
          ? `Screen sharing with audio failed: ${err.message}. Re-try and ensure "Share system audio" is enabled in the picker.`
          : 'Screen sharing with audio failed. Re-try and ensure "Share system audio" is enabled in the picker.',
        "warning"
      );
    }
  }

  async function copyInviteLink() {
    try {
      const url = new URL(window.location.href);
      url.searchParams.set("room", roomName);
      await navigator.clipboard.writeText(url.toString());
      setCopiedInvite(true);
      showToast("Invite link copied.", "success");
      window.setTimeout(() => setCopiedInvite(false), 2000);
    } catch {
      showToast("Could not copy invite link.", "warning");
    }
  }

  async function toggleRemoteFullscreen() {
    const target = remoteVideoRef.current;
    if (!target) return;
    try {
      if (document.fullscreenElement) {
        await document.exitFullscreen();
        setIsRemoteFullscreen(false);
      } else {
        await target.requestFullscreen();
        setIsRemoteFullscreen(true);
      }
    } catch {
      showToast("Fullscreen is unavailable in this context.", "warning");
    }
  }

  const statusLabel =
    callState === "connected"
      ? "Connected"
      : callState === "connecting"
        ? "Connecting"
        : callState === "reconnecting"
          ? "Reconnecting"
          : callState === "ended"
            ? "Ended"
            : "Idle";
  const onCall = Boolean(room);

  return (
    <main className="min-h-screen bg-[radial-gradient(circle_at_top,_rgba(120,140,255,0.14),_transparent_42%),linear-gradient(180deg,_#04070f_0%,_#090d18_100%)] px-4 py-6 text-slate-100">
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-4">
        <header className="flex items-center justify-between rounded-2xl border border-slate-800/70 bg-slate-950/70 px-5 py-4 backdrop-blur-md">
          <div>
            <h1 className="text-xl font-semibold tracking-tight">MyCord</h1>
            <p className="text-xs text-slate-400">Clean 1:1 voice and screen collaboration</p>
          </div>
          <div className="flex items-center gap-2">
            {nativeSession && (
              <div className="rounded-lg border border-indigo-500/40 bg-indigo-500/10 px-3 py-1 text-xs text-indigo-200">
                Native {nativeSession.backend} • {nativeSession.achievedFps.toFixed(1)}fps
              </div>
            )}
            <button
              onClick={copyInviteLink}
              className="rounded-lg border border-slate-700 bg-slate-900 px-3 py-2 text-xs text-slate-200 transition hover:border-slate-500 hover:bg-slate-800"
            >
              {copiedInvite ? "Copied" : "Copy Invite"}
            </button>
            <div
              className={classNames(
                "rounded-full px-3 py-1 text-xs font-semibold uppercase tracking-wide",
                connectionPill === "good" && "bg-emerald-500/20 text-emerald-300",
                connectionPill === "fair" && "bg-amber-500/20 text-amber-300",
                connectionPill === "poor" && "bg-rose-500/20 text-rose-300"
              )}
            >
              {connectionPill}
            </div>
          </div>
        </header>

        {!onCall && (
          <section className="grid gap-4 rounded-2xl border border-slate-800/70 bg-slate-950/70 p-5 backdrop-blur md:grid-cols-5">
            <input
              className="rounded-lg border border-slate-700 bg-slate-900 px-3 py-2 text-sm outline-none transition focus:border-indigo-400"
              placeholder="Your name"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
            />
            <input
              className="rounded-lg border border-slate-700 bg-slate-900 px-3 py-2 text-sm outline-none transition focus:border-indigo-400"
              placeholder="Room ID"
              value={roomName}
              onChange={(e) => setRoomName(e.target.value)}
            />
            <select
              value={qualityMode}
              onChange={(e) => setQualityMode(e.target.value as QualityMode)}
              className="rounded-lg border border-slate-700 bg-slate-900 px-3 py-2 text-sm outline-none transition focus:border-indigo-400"
            >
              <option value="smooth">Smooth (720p60)</option>
              <option value="balanced">Balanced (1080p60)</option>
              <option value="sharp">Sharp (1440p60)</option>
            </select>
            <select
              className="rounded-lg border border-slate-700 bg-slate-900 px-3 py-2 text-sm outline-none transition focus:border-indigo-400"
              value={selectedMicId}
              onChange={(e) => void switchMic(e.target.value)}
            >
              {audioInputs.length === 0 && <option value="">No microphone found</option>}
              {audioInputs.map((mic) => (
                <option key={mic.deviceId} value={mic.deviceId}>
                  {mic.label || `Microphone ${mic.deviceId.slice(0, 8)}`}
                </option>
              ))}
            </select>
            <button
              onClick={joinCall}
              className="rounded-lg bg-indigo-600 px-4 py-2 font-medium transition hover:bg-indigo-500"
            >
              {callState === "connecting" ? "Joining..." : "Join Room"}
            </button>
          </section>
        )}

        {onCall && (
          <>
            <section className="grid gap-4 md:grid-cols-4">
              <div className="relative rounded-2xl border border-slate-800/70 bg-slate-950/70 p-3 backdrop-blur md:col-span-3">
                <div className="mb-2 flex items-center justify-between px-1">
                  <p className="text-sm font-medium text-slate-300">
                    {remoteIdentity} {remoteIsSpeaking ? "is speaking" : ""}
                  </p>
                  <div className="flex items-center gap-2 text-xs text-slate-400">
                    {nativeSession && (
                      <>
                        <span>
                          Native {nativeSession.backend} {nativeSession.achievedFps.toFixed(1)}fps
                        </span>
                        <span>•</span>
                      </>
                    )}
                    <span>{statusLabel}</span>
                    <span>•</span>
                    <span>{formatElapsed(elapsedMs)}</span>
                  </div>
                </div>
                <video
                  ref={remoteVideoRef}
                  autoPlay
                  playsInline
                  className={classNames(
                    "aspect-video w-full rounded-xl bg-slate-900 object-contain",
                    remoteIsSpeaking && "ring-2 ring-emerald-400/80"
                  )}
                />
                <button
                  onClick={toggleRemoteFullscreen}
                  className="absolute bottom-5 right-5 rounded-lg border border-slate-700 bg-slate-900/90 px-3 py-2 text-xs text-slate-200 transition hover:border-slate-500 hover:bg-slate-800"
                >
                  {isRemoteFullscreen ? "Exit Fullscreen (F)" : "Fullscreen (F)"}
                </button>
              </div>

              <div className="rounded-2xl border border-slate-800/70 bg-slate-950/70 p-3 backdrop-blur">
                <p className="mb-2 px-1 text-sm font-medium text-slate-300">Your share preview</p>
                <video
                  ref={localVideoRef}
                  autoPlay
                  playsInline
                  muted
                  className={classNames(
                    "aspect-video w-full rounded-xl bg-slate-900 object-contain",
                    localIsSpeaking && "ring-2 ring-emerald-400/80"
                  )}
                />
                <p className="mt-2 px-1 text-xs text-slate-400">
                  {isSharingScreen ? "Screen share active" : "Not sharing your screen"}
                </p>
                {screenCaptureStats && (
                  <p className="mt-1 px-1 text-xs text-slate-500">
                    Capturing: {screenCaptureStats.width}x{screenCaptureStats.height} @{" "}
                    {screenCaptureStats.frameRate}fps
                  </p>
                )}
              </div>
            </section>

            <section className="grid gap-3 rounded-2xl border border-slate-800/70 bg-slate-950/70 p-4 backdrop-blur md:grid-cols-5">
              <button
                onClick={toggleMute}
                className={classNames(
                  "rounded-lg px-3 py-2 transition",
                  isMuted
                    ? "border border-rose-600/60 bg-rose-950/40 text-rose-200 hover:bg-rose-950/60"
                    : "border border-slate-700 bg-slate-900 text-slate-200 hover:border-slate-500 hover:bg-slate-800"
                )}
              >
                {isMuted ? "Unmute (M)" : "Mute (M)"}
              </button>
              <button
                onClick={toggleScreenShare}
                className={classNames(
                  "rounded-lg px-3 py-2 transition",
                  isSharingScreen
                    ? "border border-emerald-600/50 bg-emerald-950/30 text-emerald-200 hover:bg-emerald-950/50"
                    : "border border-slate-700 bg-slate-900 text-slate-200 hover:border-slate-500 hover:bg-slate-800"
                )}
              >
                {isSharingScreen ? "Stop Share (S)" : "Share Screen (S)"}
              </button>
              <select
                className="rounded-lg border border-slate-700 bg-slate-900 px-3 py-2 text-sm outline-none transition focus:border-indigo-400"
                value={selectedMicId}
                onChange={(e) => void switchMic(e.target.value)}
              >
                {audioInputs.length === 0 && <option value="">No microphone found</option>}
                {audioInputs.map((mic) => (
                  <option key={mic.deviceId} value={mic.deviceId}>
                    {mic.label || `Microphone ${mic.deviceId.slice(0, 8)}`}
                  </option>
                ))}
              </select>
              <button
                onClick={copyInviteLink}
                className="rounded-lg border border-slate-700 bg-slate-900 px-3 py-2 text-slate-200 transition hover:border-slate-500 hover:bg-slate-800"
              >
                {copiedInvite ? "Copied link" : "Copy invite"}
              </button>
              <button
                onClick={leaveCall}
                className="rounded-lg border border-rose-600/60 bg-rose-950/40 px-3 py-2 text-rose-200 transition hover:bg-rose-950/60"
              >
                End Call
              </button>
            </section>
          </>
        )}

        <section className="rounded-2xl border border-slate-800/70 bg-slate-950/70 px-4 py-3 text-sm text-slate-400">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <p>
              Browser: <span className="text-slate-200">{uaInfo.browser}</span> | OS:{" "}
              <span className="text-slate-200">{uaInfo.os}</span>
            </p>
            <p className="text-xs text-slate-500">{uaInfo.message}</p>
          </div>
          <div ref={remoteAudioContainerRef} />
        </section>

        {toastText && (
          <section
            className={classNames(
              "rounded-xl border px-4 py-3 text-sm",
              toastTone === "error" && "border-rose-500/40 bg-rose-950/30 text-rose-200",
              toastTone === "warning" && "border-amber-500/40 bg-amber-950/30 text-amber-200",
              toastTone === "success" && "border-emerald-500/40 bg-emerald-950/30 text-emerald-200",
              toastTone === "neutral" && "border-slate-700 bg-slate-900 text-slate-200"
            )}
          >
            {toastText}
          </section>
        )}
      </div>
    </main>
  );
}
