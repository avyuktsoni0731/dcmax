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
} from "livekit-client";
import { useEffect, useRef, useState } from "react";
import {
  BrowserName,
  OsName,
  detectBrowser,
  detectOs,
  getSystemAudioSupportMessage
} from "@/lib/capabilities";
import {
  BITRATES,
  DEFAULT_PICTURE,
  PICTURE_STORAGE_KEY,
  pictureConfig,
  PictureSettings,
  readStoredPicture,
  REFRESH_RATES,
  RESOLUTIONS,
  ResolutionId,
  RefreshRate,
  resolveCodecPreference
} from "@/lib/quality";

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
type NativeSessionState = "offline" | "stale" | "active";
type PublisherState = "starting" | "running" | "stopped" | "error";
type NativePublisher = {
  roomName: string;
  identity: string;
  state: PublisherState;
  backend: string;
  captureBackend: string;
  encoderBackend: string;
  message?: string;
  updatedAt: string;
};
type NativePublisherResponse = {
  roomName: string;
  count: number;
  publishers: NativePublisher[];
};
type NativeRuntimeStatus = "idle" | "starting" | "running" | "stopping" | "error";
type NativeRuntime = {
  roomName: string;
  identity: string;
  status: NativeRuntimeStatus;
  pid?: number;
  command: string[];
  startedAt?: string;
  stoppedAt?: string;
  lastError?: string;
  updatedAt: string;
};
type NativeRuntimeResponse = {
  roomName: string;
  runtime: NativeRuntime | null;
};
type NativeRuntimeLogEntry = {
  ts: string;
  stream: "stdout" | "stderr" | "system";
  message: string;
};
type NativeRuntimeLogsResponse = {
  roomName: string;
  count: number;
  logs: NativeRuntimeLogEntry[];
};
type NativeCaptureMode = "auto" | "scrap" | "ffmpeg-ddagrab";
type NativeEncoderMode = "fast" | "ffmpeg-libx264" | "ffmpeg-h264-nvenc";

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

const API_BASE = process.env.NEXT_PUBLIC_API_BASE_URL ?? "https://dcmax-token.soniavyukt.workers.dev";
const IS_NGROK_API = API_BASE.includes("ngrok-free.app");
const NATIVE_CONTROL_SECRET = process.env.NEXT_PUBLIC_NATIVE_CONTROL_SECRET ?? "";
const NATIVE_EXPERIMENTAL_ENABLED = process.env.NEXT_PUBLIC_ENABLE_NATIVE_EXPERIMENTAL === "true";

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

function getNativeSessionState(session: NativeSession | null): NativeSessionState {
  if (!session) return "offline";
  const ageMs = Date.now() - new Date(session.updatedAt).getTime();
  if (!Number.isFinite(ageMs) || ageMs < 0) return "stale";
  if (ageMs <= 15_000) return "active";
  return "stale";
}

async function apiFetch(input: string, init?: RequestInit): Promise<Response> {
  const headers = new Headers(init?.headers ?? {});
  if (IS_NGROK_API) {
    headers.set("ngrok-skip-browser-warning", "true");
  }
  return fetch(input, {
    ...init,
    headers
  });
}

export default function HomePage() {
  const [username, setUsername] = useState("");
  const [roomName, setRoomName] = useState("mycord-room");
  const [invite, setInvite] = useState("");
  const [callState, setCallState] = useState<CallState>("idle");
  const [room, setRoom] = useState<Room | null>(null);
  const [isMuted, setIsMuted] = useState(false);
  const [isSharingScreen, setIsSharingScreen] = useState(false);
  const [picture, setPicture] = useState<PictureSettings>(DEFAULT_PICTURE);
  const [pictureBusy, setPictureBusy] = useState(false);
  const [nameError, setNameError] = useState("");
  const [roomError, setRoomError] = useState("");
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
  const [hasRemoteVideo, setHasRemoteVideo] = useState(false);
  const [copiedInvite, setCopiedInvite] = useState(false);
  const [screenCaptureStats, setScreenCaptureStats] = useState<ScreenCaptureStats | null>(null);
  const [nativeSession, setNativeSession] = useState<NativeSession | null>(null);
  const [nativePublisher, setNativePublisher] = useState<NativePublisher | null>(null);
  const [nativeRuntime, setNativeRuntime] = useState<NativeRuntime | null>(null);
  const [nativeRuntimeLogs, setNativeRuntimeLogs] = useState<NativeRuntimeLogEntry[]>([]);
  const [nativeRuntimeBusy, setNativeRuntimeBusy] = useState(false);
  const [nativeRuntimeIdentity, setNativeRuntimeIdentity] = useState("native-sender");
  const [nativeTargetFps, setNativeTargetFps] = useState(60);
  const [nativeProbeSeconds, setNativeProbeSeconds] = useState(3);
  const [nativeHeartbeatSeconds, setNativeHeartbeatSeconds] = useState(1);
  const [nativeCaptureMode, setNativeCaptureMode] = useState<NativeCaptureMode>("auto");
  const [nativeEncoderMode, setNativeEncoderMode] = useState<NativeEncoderMode>("ffmpeg-h264-nvenc");
  const [nativeDryRun, setNativeDryRun] = useState(false);
  const [preferNativeSource, setPreferNativeSource] = useState(NATIVE_EXPERIMENTAL_ENABLED);
  const [selectedSourceLabel, setSelectedSourceLabel] = useState("none");
  const [uaInfo, setUaInfo] = useState<UaInfo>({
    browser: "other",
    os: "other",
    message: "Checking browser capabilities..."
  });

  const remoteVideoRef = useRef<HTMLVideoElement | null>(null);
  const localVideoRef = useRef<HTMLVideoElement | null>(null);
  const remoteAudioContainerRef = useRef<HTMLDivElement | null>(null);
  const nameRef = useRef<HTMLInputElement | null>(null);
  const roomRef = useRef<HTMLInputElement | null>(null);
  const remoteTrackId = useRef<string | null>(null);

  function showToast(text: string, tone: ToastTone = "neutral") {
    setToastText(text);
    setToastTone(tone);
  }

  useEffect(() => {
    const stored = readStoredPicture();
    if (stored) setPicture(stored);
    const roomParam = new URLSearchParams(window.location.search).get("room");
    if (roomParam) setRoomName(roomParam);
  }, []);

  useEffect(() => {
    window.localStorage.setItem(PICTURE_STORAGE_KEY, JSON.stringify(picture));
  }, [picture]);

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
    if (!NATIVE_EXPERIMENTAL_ENABLED) {
      setNativeSession(null);
      return;
    }
    const roomKey = roomName.trim().replace(/\s+/g, "_");
    if (!roomKey) {
      setNativeSession(null);
      return;
    }

    let active = true;
    const poll = async () => {
      try {
        const res = await apiFetch(`${API_BASE}/native/sessions/${roomKey}`);
        if (!res.ok) return;
        const contentType = res.headers.get("content-type") ?? "";
        if (!contentType.includes("application/json")) {
          showToast("Native session API returned non-JSON response. Check ngrok tunnel/warning page.", "warning");
          return;
        }
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
    }, Boolean(room) ? 1000 : 10000);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, [roomName, room]);

  useEffect(() => {
    if (!NATIVE_EXPERIMENTAL_ENABLED) {
      setNativeRuntimeLogs([]);
      return;
    }
    const roomKey = roomName.trim().replace(/\s+/g, "_");
    if (!roomKey) {
      setNativeRuntimeLogs([]);
      return;
    }
    let active = true;
    const poll = async () => {
      try {
        const res = await apiFetch(`${API_BASE}/native/runtime/${roomKey}/logs`, {
          headers: NATIVE_CONTROL_SECRET ? { "x-native-control-secret": NATIVE_CONTROL_SECRET } : undefined
        });
        if (!res.ok) return;
        const contentType = res.headers.get("content-type") ?? "";
        if (!contentType.includes("application/json")) return;
        const payload = (await res.json()) as NativeRuntimeLogsResponse;
        if (!active) return;
        setNativeRuntimeLogs(payload.logs.slice(-6));
      } catch {
        // best-effort polling
      }
    };
    void poll();
    const id = window.setInterval(() => {
      void poll();
    }, Boolean(room) ? 1000 : 5000);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, [roomName, room]);

  useEffect(() => {
    if (!NATIVE_EXPERIMENTAL_ENABLED) {
      setNativeRuntime(null);
      return;
    }
    const roomKey = roomName.trim().replace(/\s+/g, "_");
    if (!roomKey) {
      setNativeRuntime(null);
      return;
    }

    let active = true;
    const poll = async () => {
      try {
        const res = await apiFetch(`${API_BASE}/native/runtime/${roomKey}`, {
          headers: NATIVE_CONTROL_SECRET ? { "x-native-control-secret": NATIVE_CONTROL_SECRET } : undefined
        });
        if (!res.ok) return;
        const contentType = res.headers.get("content-type") ?? "";
        if (!contentType.includes("application/json")) return;
        const payload = (await res.json()) as NativeRuntimeResponse;
        if (!active) return;
        setNativeRuntime(payload.runtime);
      } catch {
        // best-effort polling
      }
    };

    void poll();
    const id = window.setInterval(() => {
      void poll();
    }, Boolean(room) ? 1000 : 10000);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, [roomName, room]);

  useEffect(() => {
    if (!NATIVE_EXPERIMENTAL_ENABLED) {
      setNativePublisher(null);
      return;
    }
    const roomKey = roomName.trim().replace(/\s+/g, "_");
    if (!roomKey) {
      setNativePublisher(null);
      return;
    }

    let active = true;
    const poll = async () => {
      try {
        const res = await apiFetch(`${API_BASE}/native/publisher/${roomKey}`);
        if (!res.ok) return;
        const contentType = res.headers.get("content-type") ?? "";
        if (!contentType.includes("application/json")) return;
        const payload = (await res.json()) as NativePublisherResponse;
        if (!active) return;
        const preferred =
          payload.publishers.find((p) => p.identity.toLowerCase().includes("native-sender")) ??
          payload.publishers[0] ??
          null;
        setNativePublisher(preferred);
      } catch {
        // best-effort polling, no user-facing toast needed
      }
    };

    void poll();
    const id = window.setInterval(() => {
      void poll();
    }, Boolean(room) ? 1000 : 10000);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, [roomName, room]);

  useEffect(() => {
    if (!room) return;
    const participants = Array.from(room.remoteParticipants.values());
    const native = participants.find((p) => p.identity.toLowerCase().includes("native"));
    const nonNative = participants.find((p) => !p.identity.toLowerCase().includes("native"));
    const preferred = preferNativeSource ? native ?? nonNative ?? null : nonNative ?? native ?? null;
    if (!preferred) return;
    setRemoteIdentity(preferred.identity);
    setSelectedSourceLabel(preferred.identity.toLowerCase().includes("native") ? "native" : "web");
    setRemoteIsSpeaking(preferred.isSpeaking);
    const publications = Array.from(preferred.trackPublications.values());
    const screenPub = publications.find(
      (p) => p.track?.kind === Track.Kind.Video && p.trackName.includes("screen")
    );
    const camPub = publications.find((p) => p.track?.kind === Track.Kind.Video);
    const targetPub = screenPub ?? camPub;
    const videoTrack = targetPub?.videoTrack;
    setHasRemoteVideo(Boolean(videoTrack));
    attachVideoOnce(videoTrack);
    if (remoteAudioContainerRef.current) {
      remoteAudioContainerRef.current.innerHTML = "";
      const audioPublications = publications.filter((pub) => pub.track?.kind === Track.Kind.Audio);
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
  }, [preferNativeSource, nativeSession, room]);

  async function requestToken(identity: string, roomToJoin: string): Promise<TokenResponse> {
    const res = await apiFetch(`${API_BASE}/token`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        identity,
        roomName: roomToJoin,
        ...(invite.trim() ? { invite: invite.trim() } : {})
      })
    });

    if (!res.ok) {
      let message = "Could not join this room.";
      try {
        const body = (await res.json()) as { error?: string };
        if (body.error) message = body.error;
      } catch {
        message = `Could not join this room (${res.status}).`;
      }
      throw new Error(message);
    }
    const contentType = res.headers.get("content-type") ?? "";
    if (!contentType.includes("application/json")) {
      throw new Error("Token API returned non-JSON response. Check ngrok tunnel/warning page.");
    }
    return (await res.json()) as TokenResponse;
  }

  function attachVideoOnce(track: { mediaStreamTrack: MediaStreamTrack; attach: (element: HTMLVideoElement) => void } | undefined) {
    const element = remoteVideoRef.current;
    if (!track || !element) {
      remoteTrackId.current = null;
      return;
    }
    if (remoteTrackId.current === track.mediaStreamTrack.id) return;
    track.attach(element);
    remoteTrackId.current = track.mediaStreamTrack.id;
  }

  function attachRemoteTrack(participant: Participant) {
    const publications = Array.from(participant.trackPublications.values());
    const screenPub = publications.find(
      (p) => p.track?.kind === Track.Kind.Video && p.trackName.includes("screen")
    );
    const camPub = publications.find((p) => p.track?.kind === Track.Kind.Video);
    const targetPub = screenPub ?? camPub;
    const track = targetPub?.videoTrack;
    setHasRemoteVideo(Boolean(track));
    attachVideoOnce(track);
  }

  function pickPreferredParticipant(roomInstance: Room): Participant | null {
    const participants = Array.from(roomInstance.remoteParticipants.values());
    if (participants.length === 0) return null;
    const native = participants.find((p) => p.identity.toLowerCase().includes("native"));
    const nonNative = participants.find((p) => !p.identity.toLowerCase().includes("native"));
    if (preferNativeSource) return native ?? nonNative ?? participants[0];
    return nonNative ?? native ?? participants[0];
  }

  function refreshPreferredRemote(roomInstance: Room) {
    const preferred = pickPreferredParticipant(roomInstance);
    if (!preferred) {
      setRemoteIdentity("Waiting for participant");
      setRemoteIsSpeaking(false);
      setHasRemoteVideo(false);
      remoteTrackId.current = null;
      setSelectedSourceLabel("none");
      if (remoteVideoRef.current) remoteVideoRef.current.srcObject = null;
      if (remoteAudioContainerRef.current) remoteAudioContainerRef.current.innerHTML = "";
      return;
    }
    setRemoteIdentity(preferred.identity);
    setSelectedSourceLabel(preferred.identity.toLowerCase().includes("native") ? "native" : "web");
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
    let roomInstance: Room | null = null;
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
      const sanitizedRoom = roomName.trim().replace(/\s+/g, "_");
      if (!identity || !sanitizedRoom) {
        setCallState("idle");
        if (!identity) {
          setNameError("Enter your name.");
          nameRef.current?.focus();
        } else {
          setNameError("");
        }
        if (!sanitizedRoom) {
          setRoomError("Enter a room code.");
          if (identity) roomRef.current?.focus();
        } else {
          setRoomError("");
        }
        return;
      }
      setNameError("");
      setRoomError("");

      const tokenData = await requestToken(identity, sanitizedRoom);
      const codecOrder = resolveCodecPreference();
      const quality = pictureConfig(picture);

      const call = new Room({
        adaptiveStream: false,
        dynacast: false,
        publishDefaults: {
          simulcast: false,
          backupCodec: false,
          degradationPreference: "maintain-framerate",
          videoCodec: codecOrder[0],
          screenShareEncoding: {
            maxBitrate: quality.maxBitrate,
            maxFramerate: quality.frameRate,
            priority: "high"
          }
        }
      });
      roomInstance = call;

      call.on(RoomEvent.ConnectionStateChanged, (state) => {
        if (state === ConnectionState.Connected) {
          setCallState("connected");
          setConnectedAt(Date.now());
        }
        if (state === ConnectionState.Disconnected) {
          setCallState("ended");
          setConnectedAt(null);
        }
      });
      call.on(RoomEvent.Reconnecting, () => setCallState("reconnecting"));
      call.on(RoomEvent.Reconnected, () => setCallState("connected"));
      call.on(RoomEvent.ParticipantConnected, (participant) => {
        setRemoteIsSpeaking(participant.isSpeaking);
        refreshPreferredRemote(call);
      });
      call.on(RoomEvent.ParticipantDisconnected, () => {
        refreshPreferredRemote(call);
      });
      call.on(RoomEvent.TrackSubscribed, () => {
        refreshPreferredRemote(call);
      });
      call.on(RoomEvent.LocalTrackPublished, () => {
        attachLocalScreen(call);
      });
      call.on(RoomEvent.ConnectionQualityChanged, (qualityValue, participant) => {
        if (participant.identity === call.localParticipant.identity) {
          setConnectionPill(qualityText(qualityValue));
        }
      });

      await call.connect(tokenData.url, tokenData.token, { autoSubscribe: true });

      try {
        if (selectedMicId) {
          await call.switchActiveDevice("audioinput", selectedMicId);
        }
        await call.localParticipant.setMicrophoneEnabled(true, {
          echoCancellation: true,
          noiseSuppression: true
        });
      } catch (micError) {
        showToast(
          micError instanceof Error
            ? `Joined without a microphone. ${micError.message}`
            : "Joined without a microphone. Allow microphone access, then pick one below.",
          "warning"
        );
      }

      const localParticipant = call.localParticipant;
      setLocalIsSpeaking(localParticipant.isSpeaking);
      localParticipant.on(ParticipantEvent.IsSpeakingChanged, () => {
        setLocalIsSpeaking(localParticipant.isSpeaking);
      });

      setRoom(call);
      setCallState("connected");
      setConnectedAt(Date.now());
      showToast("You’re in.", "success");
    } catch (err) {
      roomInstance?.disconnect();
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
    setHasRemoteVideo(false);
    remoteTrackId.current = null;
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

  async function publishScreen(roomInstance: Room, settings: PictureSettings) {
    const quality = pictureConfig(settings);
    const audioConstraints: MediaTrackConstraints = {
      echoCancellation: false,
      noiseSuppression: false,
      autoGainControl: false
    };
    await roomInstance.localParticipant.setScreenShareEnabled(
      true,
      {
        audio: audioConstraints as unknown as DisplayMediaStreamOptions["audio"],
        video: {
          width: { ideal: quality.width },
          height: { ideal: quality.height },
          frameRate: { ideal: quality.frameRate, max: quality.frameRate }
        } as { displaySurface?: "monitor" | "window" | "browser" },
        contentHint: "motion",
        selfBrowserSurface: "include",
        systemAudio: "include"
      },
      {
        simulcast: false,
        screenShareSimulcastLayers: [],
        degradationPreference: "maintain-framerate",
        screenShareEncoding: {
          maxBitrate: quality.maxBitrate,
          maxFramerate: quality.frameRate,
          priority: "high"
        }
      }
    );
    setIsSharingScreen(true);
    attachLocalScreen(roomInstance);
  }

  async function updatePicture(next: PictureSettings) {
    setPicture(next);
    if (!room || !isSharingScreen) return;
    setPictureBusy(true);
    try {
      await room.localParticipant.setScreenShareEnabled(false);
      await publishScreen(room, next);
      showToast(`Picture set to ${pictureConfig(next).summary}.`, "success");
    } catch (err) {
      setIsSharingScreen(false);
      showToast(err instanceof Error ? err.message : "Could not apply the new picture settings.", "warning");
    } finally {
      setPictureBusy(false);
    }
  }

  async function toggleScreenShare() {
    if (!room) return;
    if (!window.navigator?.mediaDevices?.getDisplayMedia) {
      showToast("Screen sharing is not available in this browser.", "warning");
      return;
    }

    try {
      if (isSharingScreen) {
        await room.localParticipant.setScreenShareEnabled(false);
        setIsSharingScreen(false);
        if (localVideoRef.current) localVideoRef.current.srcObject = null;
        setScreenCaptureStats(null);
        return;
      }

      await publishScreen(room, picture);
      showToast(`Sharing at ${pictureConfig(picture).summary}.`, "success");
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

  async function startNativeRuntime() {
    if (!NATIVE_EXPERIMENTAL_ENABLED) {
      showToast("Native experimental mode is disabled.", "warning");
      return;
    }
    const roomKey = roomName.trim().replace(/\s+/g, "_");
    if (!roomKey) {
      showToast("Enter a room name before starting native runtime.", "warning");
      return;
    }
    setNativeRuntimeBusy(true);
    try {
      const res = await apiFetch(`${API_BASE}/native/runtime/start`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          ...(NATIVE_CONTROL_SECRET ? { "x-native-control-secret": NATIVE_CONTROL_SECRET } : {})
        },
        body: JSON.stringify({
          roomName: roomKey,
          identity: nativeRuntimeIdentity.trim() || "native-sender",
          dryRun: nativeDryRun,
          targetFps: nativeTargetFps,
          probeSeconds: nativeProbeSeconds,
          heartbeatSeconds: nativeHeartbeatSeconds,
          capture: nativeCaptureMode,
          encoder: nativeEncoderMode,
          autoEnsureIngress: true
        })
      });
      if (!res.ok) {
        showToast("Failed to start native runtime. Check runtime logs section.", "error");
        return;
      }
      showToast("Native runtime start requested.", "success");
    } catch {
      showToast("Failed to reach native runtime endpoint.", "error");
    } finally {
      setNativeRuntimeBusy(false);
    }
  }

  async function stopNativeRuntime() {
    if (!NATIVE_EXPERIMENTAL_ENABLED) {
      showToast("Native experimental mode is disabled.", "warning");
      return;
    }
    const roomKey = roomName.trim().replace(/\s+/g, "_");
    if (!roomKey) return;
    setNativeRuntimeBusy(true);
    try {
      const res = await apiFetch(`${API_BASE}/native/runtime/stop`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          ...(NATIVE_CONTROL_SECRET ? { "x-native-control-secret": NATIVE_CONTROL_SECRET } : {})
        },
        body: JSON.stringify({
          roomName: roomKey
        })
      });
      if (!res.ok) {
        showToast("Failed to stop native runtime. Check runtime logs section.", "error");
        return;
      }
      showToast("Native runtime stop requested.", "success");
    } catch {
      showToast("Failed to reach native runtime endpoint.", "error");
    } finally {
      setNativeRuntimeBusy(false);
    }
  }

  const statusLabel =
    callState === "connected"
      ? "In the room"
      : callState === "connecting"
        ? "Joining…"
        : callState === "reconnecting"
          ? "Reconnecting…"
          : callState === "ended"
            ? "Ended"
            : "Idle";
  const otherLabel =
    remoteIdentity === "Waiting for participant" ? "No one else yet" : remoteIdentity.replace(/_/g, " ");
  const capture = pictureConfig(picture);
  const onCall = Boolean(room);
  const nativeSessionState = getNativeSessionState(nativeSession);
  const nativeUpdatedSeconds = nativeSession
    ? Math.max(0, Math.floor((Date.now() - new Date(nativeSession.updatedAt).getTime()) / 1000))
    : null;
  const nativePublisherAgeSeconds = nativePublisher
    ? Math.max(0, Math.floor((Date.now() - new Date(nativePublisher.updatedAt).getTime()) / 1000))
    : null;
  const nativeRuntimeAgeSeconds = nativeRuntime
    ? Math.max(0, Math.floor((Date.now() - new Date(nativeRuntime.updatedAt).getTime()) / 1000))
    : null;

  return (
    <main className="app">
      <a className="skip" href={onCall ? "#stage" : "#entry"}>
        Skip to content
      </a>

      {!onCall && (
        <section className="lobby">
          <p className="lobby-mark word" translate="no">
            MyCord
          </p>
          <div className="lobby-main" id="entry" tabIndex={-1}>
            <h1>Open a room</h1>
            <p className="lede">Voice in the room. The other person’s screen on the stage. A room code is the whole invitation.</p>
            <form
              className="entry"
              onSubmit={(event) => {
                event.preventDefault();
                void joinCall();
              }}
            >
              <label className="field">
                <span>Your name</span>
                <input
                  ref={nameRef}
                  name="name"
                  autoComplete="name"
                  spellCheck={false}
                  placeholder="Ava…"
                  value={username}
                  aria-invalid={nameError ? true : undefined}
                  aria-describedby={nameError ? "name-error" : undefined}
                  onChange={(event) => {
                    setUsername(event.target.value);
                    if (nameError) setNameError("");
                  }}
                />
                {nameError && (
                  <span id="name-error" className="field-error">
                    {nameError}
                  </span>
                )}
              </label>
              <label className="field">
                <span>Room code</span>
                <input
                  ref={roomRef}
                  name="room"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="mycord-room"
                  value={roomName}
                  aria-invalid={roomError ? true : undefined}
                  aria-describedby={roomError ? "room-error" : undefined}
                  onChange={(event) => {
                    setRoomName(event.target.value);
                    if (roomError) setRoomError("");
                  }}
                />
                {roomError && (
                  <span id="room-error" className="field-error">
                    {roomError}
                  </span>
                )}
              </label>
              <label className="field">
                <span>Invite, if this room uses one</span>
                <input
                  name="invite"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="Leave blank if you have none"
                  value={invite}
                  onChange={(event) => setInvite(event.target.value)}
                />
              </label>
              <label className="field">
                <span>Microphone</span>
                <select
                  name="microphone"
                  value={selectedMicId}
                  onChange={(event) => void switchMic(event.target.value)}
                >
                  {audioInputs.length === 0 && <option value="">No microphone found</option>}
                  {audioInputs.map((mic) => (
                    <option key={mic.deviceId} value={mic.deviceId}>
                      {mic.label || `Microphone ${mic.deviceId.slice(0, 8)}`}
                    </option>
                  ))}
                </select>
              </label>
              <div className="actions">
                <button className="btn btn-brass" type="submit" disabled={callState === "connecting"} aria-busy={callState === "connecting"}>
                  {callState === "connecting" ? "Joining…" : "Join Room"}
                </button>
                <p className="note">{uaInfo.message}</p>
              </div>
            </form>
          </div>
        </section>
      )}

      {onCall && (
        <section className="call">
          <header className="call-bar">
            <div className="who">
              <span className="word" translate="no">
                MyCord
              </span>
              <strong>{otherLabel}</strong>
              {remoteIsSpeaking && <span className="speaking">Speaking</span>}
            </div>
            <div className="meta">
              <span>{roomName}</span>
              <span>{formatElapsed(elapsedMs)}</span>
              <span>{statusLabel}</span>
              <span>{connectionPill === "good" ? "Good" : connectionPill === "fair" ? "Fair" : "Poor"}</span>
              <button type="button" className="btn" onClick={() => void toggleRemoteFullscreen()}>
                {isRemoteFullscreen ? "Exit Full Screen" : "Full Screen"}
              </button>
            </div>
          </header>

          <div className="stage" id="stage" tabIndex={-1}>
            <video ref={remoteVideoRef} className="remote" autoPlay playsInline />
            {!hasRemoteVideo && (
              <div className="empty">
                <h2>{remoteIdentity === "Waiting for participant" ? "The screen shows up here." : otherLabel}</h2>
                <p>
                  {remoteIdentity === "Waiting for participant"
                    ? "When the other person shares, it fills this stage."
                    : "Waiting for their screen."}
                </p>
              </div>
            )}
            <video
              ref={localVideoRef}
              className={isSharingScreen ? "pip" : "pip parked"}
              autoPlay
              playsInline
              muted
            />
          </div>

          <footer className="dock">
            <div className="dock-buttons">
              <button
                type="button"
                className={isMuted ? "btn btn-danger" : "btn"}
                aria-pressed={isMuted}
                onClick={() => void toggleMute()}
              >
                {isMuted ? "Unmute" : "Mute"}
              </button>
              <button
                type="button"
                className={isSharingScreen ? "btn btn-live" : "btn"}
                aria-pressed={isSharingScreen}
                onClick={() => void toggleScreenShare()}
              >
                {isSharingScreen ? "Stop Sharing" : "Share Screen"}
              </button>
              <button type="button" className="btn" onClick={() => void copyInviteLink()}>
                {copiedInvite ? "Copied" : "Copy Link"}
              </button>
              <button type="button" className="btn btn-danger" onClick={() => void leaveCall()}>
                Leave
              </button>
            </div>
            <div className="choices">
              <label className="choice">
                <span>Resolution</span>
                <select
                  name="resolution"
                  value={picture.resolutionId}
                  disabled={pictureBusy}
                  onChange={(event) =>
                    void updatePicture({ ...picture, resolutionId: event.target.value as ResolutionId })
                  }
                >
                  {RESOLUTIONS.map((item) => (
                    <option key={item.id} value={item.id}>
                      {item.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="choice">
                <span>Refresh rate</span>
                <select
                  name="refresh"
                  value={picture.refreshRate}
                  disabled={pictureBusy}
                  onChange={(event) =>
                    void updatePicture({ ...picture, refreshRate: Number(event.target.value) as RefreshRate })
                  }
                >
                  {REFRESH_RATES.map((rate) => (
                    <option key={rate} value={rate}>
                      {rate} Hz
                    </option>
                  ))}
                </select>
              </label>
              <label className="choice">
                <span>Bitrate</span>
                <select
                  name="bitrate"
                  value={picture.bitrateKbps}
                  disabled={pictureBusy}
                  onChange={(event) => void updatePicture({ ...picture, bitrateKbps: Number(event.target.value) })}
                >
                  {BITRATES.map((item) => (
                    <option key={item.kbps} value={item.kbps}>
                      {item.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="choice">
                <span>Microphone</span>
                <select name="microphone" value={selectedMicId} onChange={(event) => void switchMic(event.target.value)}>
                  {audioInputs.length === 0 && <option value="">No microphone found</option>}
                  {audioInputs.map((mic) => (
                    <option key={mic.deviceId} value={mic.deviceId}>
                      {mic.label || `Microphone ${mic.deviceId.slice(0, 8)}`}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <p className="note">
              {screenCaptureStats
                ? `Capturing ${screenCaptureStats.width}×${screenCaptureStats.height} at ${screenCaptureStats.frameRate} fps. Cap ${capture.summary}.`
                : `Picture cap ${capture.summary}. It applies when you share.`}
            </p>
          </footer>

          {NATIVE_EXPERIMENTAL_ENABLED && (
            <details className="native-fold">
              <summary>
                Native sender
                {nativeRuntime ? ` · ${nativeRuntime.status}` : ""}
                {nativeSession ? ` · ${nativeSession.achievedFps.toFixed(1)} fps · ${nativeSessionState}` : ""}
              </summary>
              <div className="native-grid">
                <input
                  aria-label="Native identity"
                  value={nativeRuntimeIdentity}
                  onChange={(event) => setNativeRuntimeIdentity(event.target.value.replace(/\s+/g, "_"))}
                  placeholder="Native identity"
                />
                <select
                  aria-label="Capture mode"
                  value={nativeCaptureMode}
                  onChange={(event) => setNativeCaptureMode(event.target.value as NativeCaptureMode)}
                >
                  <option value="auto">Capture: auto</option>
                  <option value="scrap">Capture: scrap</option>
                  <option value="ffmpeg-ddagrab">Capture: ffmpeg ddagrab</option>
                </select>
                <select
                  aria-label="Encoder"
                  value={nativeEncoderMode}
                  onChange={(event) => setNativeEncoderMode(event.target.value as NativeEncoderMode)}
                >
                  <option value="ffmpeg-h264-nvenc">NVENC</option>
                  <option value="ffmpeg-libx264">libx264</option>
                  <option value="fast">Fast</option>
                </select>
                <label>
                  <input type="checkbox" checked={nativeDryRun} onChange={(event) => setNativeDryRun(event.target.checked)} /> Dry run
                </label>
                <input
                  aria-label="Target frames per second"
                  type="number"
                  min={24}
                  max={240}
                  value={nativeTargetFps}
                  onChange={(event) => setNativeTargetFps(Math.min(240, Math.max(24, Number(event.target.value) || 60)))}
                />
                <input
                  aria-label="Probe seconds"
                  type="number"
                  min={1}
                  max={60}
                  value={nativeProbeSeconds}
                  onChange={(event) => setNativeProbeSeconds(Math.min(60, Math.max(1, Number(event.target.value) || 3)))}
                />
                <input
                  aria-label="Heartbeat seconds"
                  type="number"
                  min={1}
                  max={60}
                  value={nativeHeartbeatSeconds}
                  onChange={(event) =>
                    setNativeHeartbeatSeconds(Math.min(60, Math.max(1, Number(event.target.value) || 1)))
                  }
                />
                <button type="button" className="btn btn-live" onClick={() => setPreferNativeSource((value) => !value)}>
                  {preferNativeSource ? "Prefer the native picture" : "Prefer the browser picture"}
                </button>
                <button
                  type="button"
                  className="btn"
                  onClick={() => void startNativeRuntime()}
                  disabled={nativeRuntimeBusy || nativeRuntime?.status === "running" || nativeRuntime?.status === "starting"}
                >
                  {nativeRuntimeBusy ? "Working…" : "Start Native Sender"}
                </button>
                <button
                  type="button"
                  className="btn btn-danger"
                  onClick={() => void stopNativeRuntime()}
                  disabled={nativeRuntimeBusy || !nativeRuntime || nativeRuntime.status === "idle"}
                >
                  Stop Native Sender
                </button>
                <p>
                  {nativePublisher
                    ? `Publisher ${nativePublisher.state}${nativePublisherAgeSeconds !== null ? `, ${nativePublisherAgeSeconds}s ago` : ""}`
                    : "No publisher yet."}
                  {nativeUpdatedSeconds !== null ? ` Session ${nativeUpdatedSeconds}s ago.` : ""}
                  {nativeRuntimeAgeSeconds !== null ? ` Runtime ${nativeRuntimeAgeSeconds}s ago.` : ""}
                  {selectedSourceLabel ? ` Source ${selectedSourceLabel}.` : ""}
                </p>
                <p>{nativeRuntime?.lastError ? nativeRuntime.lastError : "No runtime error."}</p>
              </div>
              <div className="native-grid">
                {nativeRuntimeLogs.length === 0 ? (
                  <p>No runtime logs yet.</p>
                ) : (
                  nativeRuntimeLogs.map((log, index) => (
                    <p key={`${log.ts}-${index}`}>
                      {new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", second: "2-digit" }).format(
                        new Date(log.ts)
                      )}{" "}
                      [{log.stream}] {log.message}
                    </p>
                  ))
                )}
              </div>
            </details>
          )}
        </section>
      )}

      <div className="audio-sink" ref={remoteAudioContainerRef} />

      {toastText && (
        <p className={toastTone === "error" ? "toast error" : toastTone === "success" ? "toast success" : "toast"} role="status" aria-live="polite">
          {toastText}
        </p>
      )}
    </main>
  );
}
