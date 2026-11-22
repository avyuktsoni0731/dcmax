let context: AudioContext | null = null;

/** Call this from a click so later tones are allowed to play. */
export function primeAudio() {
  const audio = context ?? new AudioContext();
  context = audio;
  if (audio.state === "suspended") {
    void audio.resume();
  }
}

function tone(frequency: number, duration: number, type: OscillatorType, level: number, when = 0) {
  if (!context) return;
  const audio = context;
  const start = audio.currentTime + when;
  const oscillator = audio.createOscillator();
  const gain = audio.createGain();
  oscillator.type = type;
  oscillator.frequency.setValueAtTime(frequency, start);
  gain.gain.setValueAtTime(0.0001, start);
  gain.gain.exponentialRampToValueAtTime(level, start + 0.02);
  gain.gain.exponentialRampToValueAtTime(0.0001, start + duration);
  oscillator.connect(gain);
  gain.connect(audio.destination);
  oscillator.start(start);
  oscillator.stop(start + duration + 0.02);
}

export function playJoined() {
  tone(523, 0.12, "sine", 0.04);
  tone(784, 0.16, "sine", 0.032, 0.09);
}

export function playLeft() {
  tone(392, 0.12, "sine", 0.032);
  tone(262, 0.18, "sine", 0.026, 0.08);
}

export function playEnter() {
  tone(659, 0.1, "sine", 0.034);
  tone(880, 0.14, "sine", 0.028, 0.08);
}

export function playExit() {
  tone(440, 0.16, "triangle", 0.028);
}

export function playMute() {
  tone(330, 0.07, "sine", 0.03);
}

export function playUnmute() {
  tone(523, 0.08, "sine", 0.03);
}
