import type { ReactNode } from "react";

/** Material Icons, drawn inline so the desktop window does not load a font. */

type IconProps = { size?: number };

function Glyph({ size = 22, children }: IconProps & { children: ReactNode }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      {children}
    </svg>
  );
}

function Fill({ d }: { d: string }) {
  return <path fill="currentColor" d={d} />;
}

export function MicIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M12 14c1.66 0 2.99-1.34 2.99-3L15 5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm5.3-3c0 3-2.54 5.1-5.3 5.1S6.7 14 6.7 11H5c0 3.41 2.72 6.23 6 6.72V21h2v-3.28c3.28-.48 6-3.3 6-6.72h-1.7z" />
    </Glyph>
  );
}

export function MicOffIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M19 11h-1.7c0 .74-.16 1.43-.43 2.05l1.23 1.23c.56-.98.9-2.09.9-3.28zm-4.02.17c0-.06.02-.11.02-.17V5c0-1.66-1.34-3-3-3S9 3.34 9 5v.18l5.98 5.99zM4.27 3 3 4.27l6.01 6.01V11c0 1.66 1.33 3 2.99 3 .22 0 .44-.03.65-.08l1.66 1.66c-.71.33-1.5.52-2.31.52-2.76 0-5.3-2.1-5.3-5.1H5c0 3.41 2.72 6.23 6 6.72V21h2v-3.28c.91-.13 1.77-.45 2.54-.9L19.73 21 21 19.73 4.27 3z" />
    </Glyph>
  );
}

export function StopShareIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M21.22 18.02l2 2H24v-2h-2.78zm.77-2l.01-10c0-1.11-.9-2-2-2H7.22l5.23 5.23c.18-.04.36-.07.55-.1V7.02l4 3.73-1.58 1.47 5.54 5.54c.61-.33 1.03-.99 1.03-1.74zM2.39 1.73L1.11 3l1.54 1.54c-.4.36-.65.89-.65 1.48v10c0 1.1.89 2 2 2H0v2h18.13l2.71 2.71 1.27-1.27L2.39 1.73zM7 15.02c.31-1.48.92-2.95 2.07-4.06l1.59 1.59c-1.54.38-2.7 1.18-3.66 2.47z" />
    </Glyph>
  );
}

export function ShareIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M20 18c1.1 0 1.99-.9 1.99-2L22 6c0-1.11-.9-2-2-2H4c-1.11 0-2 .89-2 2v10c0 1.1.89 2 2 2H0v2h24v-2h-4zm-7-3.53v-2.19c-2.78 0-4.61.85-6 2.72.56-2.67 2.11-5.33 6-5.87V7l4 3.73-4 3.74z" />
    </Glyph>
  );
}

export function StopIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M6 6h12v12H6z" />
    </Glyph>
  );
}

export function FullscreenIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z" />
    </Glyph>
  );
}

export function FullscreenExitIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M5 16h3v3h2v-5H5v2zm3-8H5v2h5V5H8v3zm6 11h2v-3h3v-2h-5v5zm2-11V5h-2v5h5V8h-3z" />
    </Glyph>
  );
}

export function LeaveIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M12 9c-1.6 0-3.15.25-4.6.72v3.1c0 .39-.23.74-.56.9-.98.49-1.87 1.12-2.66 1.85-.18.18-.43.28-.7.28-.28 0-.53-.11-.71-.29L.29 13.08a.996.996 0 0 1 0-1.41C3.34 8.78 7.46 7 12 7s8.66 1.78 11.71 4.67c.18.18.29.43.29.71 0 .28-.11.53-.29.71l-2.48 2.48c-.18.18-.43.29-.71.29-.27 0-.52-.11-.7-.28a11.27 11.27 0 0 0-2.67-1.85c-.33-.16-.56-.5-.56-.9v-3.1C15.15 9.25 13.6 9 12 9z" />
    </Glyph>
  );
}

export function DarkIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M12 3a9 9 0 1 0 9 9c0-.46-.04-.92-.1-1.36a5.389 5.389 0 0 1-4.4 2.26 5.403 5.403 0 0 1-5.4-5.4c0-1.81.89-3.42 2.26-4.4-.44-.06-.9-.1-1.36-.1z" />
    </Glyph>
  );
}

export function HomeIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M10 20v-6h4v6h5v-8h3L12 3 2 12h3v8z" />
    </Glyph>
  );
}

export function FriendsIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M16 11c1.66 0 2.99-1.34 2.99-3S17.66 5 16 5c-1.66 0-3 1.34-3 3s1.34 3 3 3zm-8 0c1.66 0 2.99-1.34 2.99-3S9.66 5 8 5C6.34 5 5 6.34 5 8s1.34 3 3 3zm0 2c-2.33 0-7 1.17-7 3.5V19h14v-2.5c0-2.33-4.67-3.5-7-3.5zm8 0c-.29 0-.62.02-.97.05 1.16.84 1.97 1.97 1.97 3.45V19h6v-2.5c0-2.33-4.67-3.5-7-3.5z" />
    </Glyph>
  );
}

export function RecentsIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M11.99 2C6.47 2 2 6.48 2 12s4.47 10 9.99 10C17.52 22 22 17.52 22 12S17.52 2 11.99 2zM12 20c-4.42 0-8-3.58-8-8s3.58-8 8-8 8 3.58 8 8-3.58 8-8 8zm.5-13H11v6l5.25 3.15.75-1.23-4.5-2.67z" />
    </Glyph>
  );
}

export function MessagesIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M20 2H4c-1.1 0-1.99.9-1.99 2L2 22l4-4h14c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm-2 12H6v-2h12v2zm0-3H6V9h12v2zm0-3H6V6h12v2z" />
    </Glyph>
  );
}

export function SettingsIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M19.14 12.94c.04-.31.06-.63.06-.94s-.02-.63-.06-.94l2.03-1.58a.49.49 0 0 0 .12-.61l-1.92-3.32a.488.488 0 0 0-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54a.484.484 0 0 0-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96c-.22-.08-.47 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.04.31-.06.63-.06.94s.02.63.06.94l-2.03 1.58a.49.49 0 0 0-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6A3.6 3.6 0 1 1 12 8.4a3.6 3.6 0 0 1 0 7.2z" />
    </Glyph>
  );
}

export function PhoneIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M6.62 10.79c1.44 2.83 3.76 5.14 6.59 6.59l2.2-2.2c.27-.27.67-.36 1.02-.24 1.12.37 2.33.57 3.57.57.55 0 1 .45 1 1V20c0 .55-.45 1-1 1-9.39 0-17-7.61-17-17 0-.55.45-1 1-1h3.5c.55 0 1 .45 1 1 0 1.25.2 2.45.57 3.57.11.35.03.74-.25 1.02l-2.2 2.2z" />
    </Glyph>
  );
}

export function MonitorIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M21 2H3c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h7v2H8v2h8v-2h-2v-2h7c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm0 14H3V4h18v12z" />
    </Glyph>
  );
}

export function AddIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z" />
    </Glyph>
  );
}

export function SearchIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M15.5 14h-.79l-.28-.27A6.471 6.471 0 0 0 16 9.5 6.5 6.5 0 1 0 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z" />
    </Glyph>
  );
}

export function SendIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M2.01 21 23 12 2.01 3 2 10l15 2-15 2z" />
    </Glyph>
  );
}

export function ChevronIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M7.41 8.59 12 13.17l4.59-4.58L18 10l-6 6-6-6z" />
    </Glyph>
  );
}

export function HeadphonesOffIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M12 4c3.87 0 7 3.13 7 7v2h-2.92L21 17.92V11c0-4.97-4.03-9-9-9-1.95 0-3.76.62-5.23 1.68l1.44 1.44C9.3 4.41 10.6 4 12 4zM2.27 1.72L1 3l3.33 3.32C3.49 7.68 3 9.29 3 11v7c0 1.66 1.34 3 3 3h3v-8H5v-2c0-1.17.29-2.26.79-3.22L15 17v4h3c.3 0 .59-.06.86-.14L21 23l1.27-1.27-20-20.01z" />
    </Glyph>
  );
}

export function HeadphonesIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M12 1a9 9 0 0 0-9 9v7c0 1.66 1.34 3 3 3h3v-8H5v-2c0-3.87 3.13-7 7-7s7 3.13 7 7v2h-4v8h3c1.66 0 3-1.34 3-3v-7a9 9 0 0 0-9-9z" />
    </Glyph>
  );
}

export function ArrowIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M10 6 8.59 7.41 13.17 12l-4.58 4.59L10 18l6-6z" />
    </Glyph>
  );
}

export function ChatBubbleIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M20 2H4c-1.1 0-2 .9-2 2v18l4-4h14c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm0 14H6l-2 2V4h16v12z" />
    </Glyph>
  );
}

export function ForumIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M21 6h-2v9H6v2c0 .55.45 1 1 1h11l4 4V7c0-.55-.45-1-1-1zm-4 6V3c0-.55-.45-1-1-1H3c-.55 0-1 .45-1 1v14l4-4h10c.55 0 1-.45 1-1z" />
    </Glyph>
  );
}

export function PhoneInTalkIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M20 15.5c-1.25 0-2.45-.2-3.57-.57-.35-.11-.74-.03-1.02.24l-2.2 2.2c-2.83-1.44-5.15-3.75-6.59-6.59l2.2-2.21c.28-.26.36-.65.25-1C8.7 6.45 8.5 5.25 8.5 4c0-.55-.45-1-1-1H4c-.55 0-1 .45-1 1 0 9.39 7.61 17 17 17 .55 0 1-.45 1-1v-3.5c0-.55-.45-1-1-1zM19 12h2c0-4.97-4.03-9-9-9v2c3.87 0 7 3.13 7 7zm-4 0h2c0-2.76-2.24-5-5-5v2c1.66 0 3 1.34 3 3z" />
    </Glyph>
  );
}

export function PersonSearchIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <circle cx="10" cy="8" r="4" fill="currentColor" />
      <path fill="currentColor" d="M10.35 14.01C7.62 13.91 2 15.27 2 18v2h9.54C9.07 17.24 10.31 14.11 10.35 14.01z" />
      <path fill="currentColor" fillRule="evenodd" d="M19.43 18.02C19.79 17.43 20 16.74 20 16c0-2.21-1.79-4-4-4s-4 1.79-4 4 1.79 4 4 4c.74 0 1.43-.22 2.02-.57L20.59 22 22 20.59 19.43 18.02zM16 18c-1.1 0-2-.9-2-2 0-1.1.9-2 2-2s2 .9 2 2c0 1.1-.9 2-2 2z" />
    </Glyph>
  );
}

export function EmojiPeopleIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M12 6c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2v1.5l-1.59-1.59a.996.996 0 1 0-1.41 1.41L10.17 14H8.5c-.83 0-1.5.67-1.5 1.5S7.67 17 8.5 17H11v5h2v-5h2.5c.83 0 1.5-.67 1.5-1.5s-.67-1.5-1.5-1.5h-1.67l3.17-3.18a.996.996 0 1 0-1.41-1.41L14 11.5V10c0-1.1-.9-2-2-2z" />
    </Glyph>
  );
}

export function VideoIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M17 10.5V7c0-.55-.45-1-1-1H4c-.55 0-1 .45-1 1v10c0 .55.45 1 1 1h12c.55 0 1-.45 1-1v-3.5l4 4v-11l-4 4z" />
    </Glyph>
  );
}

export function SmileIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M11.99 2C6.47 2 2 6.48 2 12s4.47 10 9.99 10C17.52 22 22 17.52 22 12S17.52 2 11.99 2zM12 20c-4.42 0-8-3.58-8-8s3.58-8 8-8 8 3.58 8 8-3.58 8-8 8zm3.5-9c.83 0 1.5-.67 1.5-1.5S16.33 8 15.5 8 14 8.67 14 9.5s.67 1.5 1.5 1.5zm-7 0c.83 0 1.5-.67 1.5-1.5S9.33 8 8.5 8 7 8.67 7 9.5 7.67 11 8.5 11zm3.5 6.5c2.33 0 4.31-1.46 5.11-3.5H6.89c.8 2.04 2.78 3.5 5.11 3.5z" />
    </Glyph>
  );
}

export function ChevronRightIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M10 6 8.59 7.41 13.17 12l-4.58 4.59L10 18l6-6z" />
    </Glyph>
  );
}

export function VisibilityIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M12 4.5C7 4.5 2.73 7.61 1 12c1.73 4.39 6 7.5 11 7.5s9.27-3.11 11-7.5c-1.73-4.39-6-7.5-11-7.5zM12 17c-2.76 0-5-2.24-5-5s2.24-5 5-5 5 2.24 5 5-2.24 5-5 5zm0-8c-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3-1.34-3-3-3z" />
    </Glyph>
  );
}

export function VisibilityOffIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M12 7c2.76 0 5 2.24 5 5 0 .65-.13 1.26-.36 1.83l2.92 2.92c1.51-1.26 2.7-2.89 3.43-4.75-1.73-4.39-6-7.5-11-7.5-1.4 0-2.74.25-3.98.7l2.16 2.16C10.74 7.13 11.35 7 12 7zM2 4.27l2.28 2.28.46.46C3.08 8.3 1.78 10.02 1 12c1.73 4.39 6 7.5 11 7.5 1.55 0 3.03-.3 4.38-.84l.42.42L19.73 22 21 20.73 3.27 3 2 4.27zM7.53 9.8l1.55 1.55c-.05.21-.08.43-.08.65 0 1.66 1.34 3 3 3 .22 0 .44-.03.65-.08l1.55 1.55c-.67.33-1.41.53-2.2.53-2.76 0-5-2.24-5-5 0-.79.2-1.53.53-2.2zm4.31-.78 3.15 3.15.02-.16c0-1.66-1.34-3-3-3l-.17.01z" />
    </Glyph>
  );
}

export function PersonAddIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M15 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm-9-2V7H4v3H1v2h3v3h2v-3h3v-2H6zm9 4c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z" />
    </Glyph>
  );
}

export function CheckIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M9 16.17 4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z" />
    </Glyph>
  );
}

export function CloseIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M19 6.41 17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z" />
    </Glyph>
  );
}

export function RamenMark({ size = 150 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 300 300" aria-hidden="true">
      <path fill="#ffffff" d="M125 250H175V236.25L190.625 230C204.208 224.583 215.708 217.042 225.125 207.375C234.625 197.708 241.458 186.917 245.625 175H54.375C58.5417 186.833 65.3333 197.625 74.75 207.375C84.1667 217.125 95.7083 224.667 109.375 230L125 236.25V250ZM100 275V253.125C77.75 244.375 59.6667 230.833 45.75 212.5C31.8333 194.167 24.9167 173.333 25 150H50V50L275 25V43.75L131.25 60V81.25H275V100H131.25V150H275C275 173.333 268.083 194.167 254.25 212.5C240.417 230.833 222.333 244.375 200 253.125V275H100ZM100 81.25H112.5V61.875L100 63.375V81.25ZM68.75 81.25H81.25V65.625L68.75 66.875V81.25ZM100 150H112.5V100H100V150ZM68.75 150H81.25V100H68.75V150Z" />
    </svg>
  );
}

export function LightIcon(props: IconProps) {
  return (
    <Glyph {...props}>
      <Fill d="M12 7c-2.76 0-5 2.24-5 5s2.24 5 5 5 5-2.24 5-5-2.24-5-5-5zM2 13h2c.55 0 1-.45 1-1s-.45-1-1-1H2c-.55 0-1 .45-1 1s.45 1 1 1zm18 0h2c.55 0 1-.45 1-1s-.45-1-1-1h-2c-.55 0-1 .45-1 1s.45 1 1 1zM11 2v2c0 .55.45 1 1 1s1-.45 1-1V2c0-.55-.45-1-1-1s-1 .45-1 1zm0 18v2c0 .55.45 1 1 1s1-.45 1-1v-2c0-.55-.45-1-1-1s-1 .45-1 1zM5.99 4.58a.996.996 0 0 0-1.41 0c-.39.39-.39 1.03 0 1.41l1.06 1.06c.39.39 1.03.39 1.41 0s.39-1.03 0-1.41L5.99 4.58zm12.37 12.37a.996.996 0 0 0-1.41 0c-.39.39-.39 1.03 0 1.41l1.06 1.06c.39.39 1.03.39 1.41 0 .39-.39.39-1.03 0-1.41l-1.06-1.06zm1.06-10.96a.996.996 0 0 0 0-1.41c-.39-.39-1.03-.39-1.41 0l-1.06 1.06c-.39.39-.39 1.03 0 1.41s1.03.39 1.41 0l1.06-1.06zM7.05 18.36a.996.996 0 0 0 0-1.41c-.39-.39-1.03-.39-1.41 0l-1.06 1.06c-.39.39-.39 1.03 0 1.41s1.03.39 1.41 0l1.06-1.06z" />
    </Glyph>
  );
}
