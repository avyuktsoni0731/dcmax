"use client";

import {
  LocalTrackPublication,
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

type UaInfo = {
  browser: BrowserName;
  os: OsName;
  message: string;
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
  const [remoteIsSpeaking, setRemoteIsSpeaking] = useState(false);
  const [localIsSpeaking, setLocalIsSpeaking] = useState(false);
  const [connectionPill, setConnectionPill] = useState<"good" | "fair" | "poor">("good");
  const [errorText, setErrorText] = useState("");
  const [uaInfo, setUaInfo] = useState<UaInfo>({
    browser: "other",
    os: "other",
    message: "Checking browser capabilities..."
  });

  const remoteVideoRef = useRef<HTMLVideoElement | null>(null);
  const localVideoRef = useRef<HTMLVideoElement | null>(null);
  const remoteAudioContainerRef = useRef<HTMLDivElement | null>(null);

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
      setErrorText(`This browser/context does not expose media devices.${secureHint}`);
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
        setErrorText("Unable to read media devices. Check site permissions for microphone access.");
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
      remoteAudioContainerRef.current.appendChild(el);
      void el.play().catch(() => {
        setErrorText("Remote audio is blocked by autoplay policy. Click anywhere and try again.");
      });
    }
  }

  function attachLocalScreen(roomInstance: Room) {
    const screenPub = Array.from(roomInstance.localParticipant.videoTrackPublications.values()).find((p) =>
      p.trackName.includes("screen")
    );
    if (screenPub?.videoTrack && localVideoRef.current) {
      screenPub.videoTrack.attach(localVideoRef.current);
    } else if (localVideoRef.current) {
      localVideoRef.current.srcObject = null;
    }
  }

  async function joinCall() {
    setErrorText("");
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
        if (state === ConnectionState.Connected) setCallState("connected");
        if (state === ConnectionState.Disconnected) setCallState("ended");
      });
      roomInstance.on(RoomEvent.Reconnecting, () => setCallState("reconnecting"));
      roomInstance.on(RoomEvent.Reconnected, () => setCallState("connected"));
      roomInstance.on(RoomEvent.ParticipantConnected, (participant) => {
        setRemoteIsSpeaking(participant.isSpeaking);
        attachRemoteTrack(participant);
        attachRemoteAudio(participant);
      });
      roomInstance.on(RoomEvent.ParticipantDisconnected, () => {
        setRemoteIsSpeaking(false);
        if (remoteVideoRef.current) remoteVideoRef.current.srcObject = null;
        if (remoteAudioContainerRef.current) remoteAudioContainerRef.current.innerHTML = "";
      });
      roomInstance.on(RoomEvent.TrackSubscribed, (_track, _publication, participant) => {
        if (participant.identity === roomInstance.remoteParticipants.values().next().value?.identity) {
          attachRemoteTrack(participant);
          attachRemoteAudio(participant);
        }
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
    } catch (err) {
      setCallState("idle");
      setErrorText(err instanceof Error ? err.message : "Failed to join call");
    }
  }

  async function leaveCall() {
    if (!room) return;
    room.disconnect();
    setRoom(null);
    setCallState("ended");
    setIsSharingScreen(false);
    setRemoteIsSpeaking(false);
    if (remoteVideoRef.current) remoteVideoRef.current.srcObject = null;
    if (localVideoRef.current) localVideoRef.current.srcObject = null;
  }

  async function toggleMute() {
    if (!room) return;
    const local = room.localParticipant as LocalParticipant;
    await local.setMicrophoneEnabled(isMuted);
    setIsMuted((v) => !v);
  }

  async function switchMic(micId: string) {
    setSelectedMicId(micId);
    if (!room) return;
    await room.switchActiveDevice("audioinput", micId);
  }

  async function toggleScreenShare() {
    if (!room) return;
    if (!window.navigator?.mediaDevices?.getDisplayMedia) {
      setErrorText("Screen sharing is not available in this browser.");
      return;
    }

    const quality = QUALITY_PROFILES[qualityMode];
    try {
      if (isSharingScreen) {
        await room.localParticipant.setScreenShareEnabled(false);
        setIsSharingScreen(false);
        if (localVideoRef.current) localVideoRef.current.srcObject = null;
        return;
      }

      const localParticipant = room.localParticipant;
      const publishOptions = {
        screenShareEncoding: {
          maxBitrate: quality.maxBitrate,
          maxFramerate: quality.frameRate
        }
      };

      try {
        await localParticipant.setScreenShareEnabled(
          true,
          {
            audio: true,
            selfBrowserSurface: "include"
          },
          publishOptions
        );
      } catch {
        try {
          await localParticipant.setScreenShareEnabled(true, { audio: true }, publishOptions);
          setErrorText("Screen sharing started, but advanced browser surface options were skipped.");
        } catch {
          await localParticipant.setScreenShareEnabled(true, { audio: false }, publishOptions);
          setErrorText(
            "Screen sharing started without audio. Windows/browser may not support system audio for this capture."
          );
        }
      }
      setIsSharingScreen(true);
      attachLocalScreen(room);
    } catch (err) {
      setErrorText(
        err instanceof Error
          ? `Screen sharing failed: ${err.message}`
          : "Screen sharing failed. Check browser permissions and OS support."
      );
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

  return (
    <main className="mx-auto flex min-h-screen w-full max-w-6xl flex-col gap-6 px-4 py-8">
      <header className="flex items-center justify-between rounded-xl bg-slate-900/70 px-4 py-3">
        <div>
          <h1 className="text-xl font-semibold text-slate-100">MyCord Web</h1>
          <p className="text-sm text-slate-400">Local-first 1:1 voice and screen sharing</p>
        </div>
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
      </header>

      <section className="grid gap-4 rounded-xl bg-slate-900/70 p-4 md:grid-cols-4">
        <input
          className="rounded-lg border border-slate-700 bg-slate-950 px-3 py-2"
          placeholder="Username"
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          disabled={Boolean(room)}
        />
        <input
          className="rounded-lg border border-slate-700 bg-slate-950 px-3 py-2"
          placeholder="Room"
          value={roomName}
          onChange={(e) => setRoomName(e.target.value)}
          disabled={Boolean(room)}
        />
        <select
          value={qualityMode}
          onChange={(e) => setQualityMode(e.target.value as QualityMode)}
          className="rounded-lg border border-slate-700 bg-slate-950 px-3 py-2"
          disabled={Boolean(room)}
        >
          <option value="smooth">Smooth (720p60)</option>
          <option value="balanced">Balanced (1080p60)</option>
          <option value="sharp">Sharp (1440p60)</option>
        </select>
        <div className="flex items-center gap-2">
          {!room ? (
            <button
              onClick={joinCall}
              className="w-full rounded-lg bg-indigo-600 px-4 py-2 font-medium hover:bg-indigo-500"
            >
              Join
            </button>
          ) : (
            <button
              onClick={leaveCall}
              className="w-full rounded-lg bg-rose-600 px-4 py-2 font-medium hover:bg-rose-500"
            >
              Leave
            </button>
          )}
        </div>
      </section>

      <section className="grid gap-4 md:grid-cols-3">
        <div className="rounded-xl bg-slate-900/70 p-4 md:col-span-2">
          <div className="mb-2 flex items-center justify-between">
            <p className="text-sm font-medium text-slate-300">
              Remote stream {remoteIsSpeaking ? " - speaking" : ""}
            </p>
            <span className="text-xs text-slate-400">{statusLabel}</span>
          </div>
          <video
            ref={remoteVideoRef}
            autoPlay
            playsInline
            className={classNames(
              "aspect-video w-full rounded-lg bg-slate-950 object-contain",
              remoteIsSpeaking && "ring-2 ring-emerald-400"
            )}
          />
        </div>
        <div className="rounded-xl bg-slate-900/70 p-4">
          <p className="mb-2 text-sm font-medium text-slate-300">
            Your shared screen {localIsSpeaking ? " - speaking" : ""}
          </p>
          <video
            ref={localVideoRef}
            autoPlay
            playsInline
            muted
            className={classNames(
              "aspect-video w-full rounded-lg bg-slate-950 object-contain",
              localIsSpeaking && "ring-2 ring-emerald-400"
            )}
          />
        </div>
      </section>

      <section className="grid gap-4 rounded-xl bg-slate-900/70 p-4 md:grid-cols-4">
        <button
          onClick={toggleMute}
          disabled={!room}
          className="rounded-lg bg-slate-800 px-3 py-2 hover:bg-slate-700 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {isMuted ? "Unmute" : "Mute"}
        </button>
        <button
          onClick={toggleScreenShare}
          disabled={!room}
          className="rounded-lg bg-slate-800 px-3 py-2 hover:bg-slate-700 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {isSharingScreen ? "Stop Share" : "Share Screen"}
        </button>
        <select
          className="rounded-lg border border-slate-700 bg-slate-950 px-3 py-2"
          disabled={!room}
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
        <div className="rounded-lg border border-slate-800 bg-slate-950 px-3 py-2 text-xs text-slate-400">
          {uaInfo.message}
        </div>
      </section>

      {errorText && (
        <section className="rounded-lg border border-rose-500/40 bg-rose-950/30 px-4 py-3 text-sm text-rose-200">
          {errorText}
        </section>
      )}

      <section className="rounded-xl bg-slate-900/70 px-4 py-3 text-sm text-slate-400">
        <p>
          Browser: <span className="text-slate-200">{uaInfo.browser}</span> | OS:{" "}
          <span className="text-slate-200">{uaInfo.os}</span>
        </p>
        <div ref={remoteAudioContainerRef} />
      </section>
    </main>
  );
}
