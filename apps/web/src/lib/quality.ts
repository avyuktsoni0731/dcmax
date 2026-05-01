import { VideoCodec } from "livekit-client";

export type QualityMode = "smooth" | "balanced" | "sharp";

type QualityConfig = {
  width: number;
  height: number;
  frameRate: number;
  maxBitrate: number;
};

export const QUALITY_PROFILES: Record<QualityMode, QualityConfig> = {
  smooth: {
    width: 1280,
    height: 720,
    frameRate: 60,
    maxBitrate: 3_000_000
  },
  balanced: {
    width: 1920,
    height: 1080,
    frameRate: 60,
    maxBitrate: 8_000_000
  },
  sharp: {
    width: 2560,
    height: 1440,
    frameRate: 60,
    maxBitrate: 12_000_000
  }
};

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
