import { VideoCodec } from "livekit-client";

export type ResolutionId = "720" | "1080" | "1440";
export type RefreshRate = 30 | 60 | 120;

export type PictureSettings = {
  resolutionId: ResolutionId;
  refreshRate: RefreshRate;
  bitrateKbps: number;
};

export const RESOLUTIONS: Array<{ id: ResolutionId; width: number; height: number; label: string }> = [
  { id: "720", width: 1280, height: 720, label: "1280 × 720" },
  { id: "1080", width: 1920, height: 1080, label: "1920 × 1080" },
  { id: "1440", width: 2560, height: 1440, label: "2560 × 1440" }
];

export const REFRESH_RATES: RefreshRate[] = [30, 60, 120];

export const BITRATES: Array<{ kbps: number; label: string }> = [
  { kbps: 8_000, label: "8 Mbps" },
  { kbps: 12_000, label: "12 Mbps" },
  { kbps: 20_000, label: "20 Mbps" },
  { kbps: 40_000, label: "40 Mbps" },
  { kbps: 80_000, label: "80 Mbps" },
  { kbps: 100_000, label: "100 Mbps" }
];

export const PICTURE_STORAGE_KEY = "mycord-picture-v2";

export const DEFAULT_PICTURE: PictureSettings = {
  resolutionId: "1080",
  refreshRate: 30,
  bitrateKbps: 8_000
};

export function pictureConfig(settings: PictureSettings) {
  const resolution = RESOLUTIONS.find((item) => item.id === settings.resolutionId) ?? RESOLUTIONS[1];
  const bitrate = BITRATES.find((item) => item.kbps === settings.bitrateKbps) ?? BITRATES[2];
  return {
    width: resolution.width,
    height: resolution.height,
    frameRate: settings.refreshRate,
    maxBitrate: bitrate.kbps * 1000,
    summary: `${resolution.label} · ${settings.refreshRate} Hz · ${bitrate.label}`
  };
}

export function readStoredPicture(): PictureSettings | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(PICTURE_STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<PictureSettings>;
    const resolutionId = RESOLUTIONS.some((item) => item.id === parsed.resolutionId)
      ? (parsed.resolutionId as ResolutionId)
      : null;
    const refreshRate = REFRESH_RATES.includes(parsed.refreshRate as RefreshRate)
      ? (parsed.refreshRate as RefreshRate)
      : null;
    const bitrateKbps = BITRATES.some((item) => item.kbps === parsed.bitrateKbps) ? parsed.bitrateKbps ?? null : null;
    if (!resolutionId || !refreshRate || !bitrateKbps) return null;
    return { resolutionId, refreshRate, bitrateKbps };
  } catch {
    return null;
  }
}

export function resolveCodecPreference(): VideoCodec[] {
  const capabilities = RTCRtpSender.getCapabilities("video")?.codecs ?? [];
  const availableMime = capabilities.map((codec) => codec.mimeType.toLowerCase());
  const preferred: VideoCodec[] = [];

  if (availableMime.some((v) => v.includes("h264"))) {
    preferred.push("h264");
  }
  if (availableMime.some((v) => v.includes("vp9"))) {
    preferred.push("vp9");
  }
  if (availableMime.some((v) => v.includes("av1"))) {
    preferred.push("av1");
  }
  if (availableMime.some((v) => v.includes("vp8"))) {
    preferred.push("vp8");
  }

  return preferred.length > 0 ? preferred : ["h264", "vp9", "vp8"];
}
