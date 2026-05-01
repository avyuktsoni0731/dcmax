export type BrowserName = "chrome" | "edge" | "firefox" | "safari" | "other";
export type OsName = "windows" | "macos" | "linux" | "other";

export function detectBrowser(userAgent: string): BrowserName {
  const ua = userAgent.toLowerCase();
  if (ua.includes("edg/")) return "edge";
  if (ua.includes("chrome/")) return "chrome";
  if (ua.includes("firefox/")) return "firefox";
  if (ua.includes("safari/")) return "safari";
  return "other";
}

export function detectOs(userAgent: string): OsName {
  const ua = userAgent.toLowerCase();
  if (ua.includes("windows")) return "windows";
  if (ua.includes("mac os")) return "macos";
  if (ua.includes("linux")) return "linux";
  return "other";
}

export function getSystemAudioSupportMessage(browser: BrowserName, os: OsName): string {
  if ((browser === "chrome" || browser === "edge") && os === "windows") {
    return "System audio is usually supported for full-screen sharing.";
  }
  if ((browser === "chrome" || browser === "edge") && os === "macos") {
    return "Full system audio is limited on macOS. Tab audio works more reliably.";
  }
  if (browser === "firefox") {
    return "Firefox has limited screen audio support. Use Chrome/Edge for best results.";
  }
  if (browser === "safari") {
    return "Safari does not reliably support screen audio capture. Use Chrome/Edge.";
  }
  return "Screen audio support depends on browser and OS. Chrome/Edge recommended.";
}
