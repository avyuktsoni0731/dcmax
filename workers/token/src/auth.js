// Account checks for the token worker. Passwords stay hashed. The call
// shows the username, and the email is only used to sign in.

const encoder = new TextEncoder();
const MONTHS = [
  "January",
  "February",
  "March",
  "April",
  "May",
  "June",
  "July",
  "August",
  "September",
  "October",
  "November",
  "December",
];
const ITERATIONS = 100_000;

export function databaseName(uri) {
  try {
    const parsed = new URL(String(uri).replace(/^mongodb(\+srv)?:\/\//, "http://"));
    const name = decodeURIComponent(parsed.pathname.replace(/^\//, "")).split("/")[0];
    if (/^[A-Za-z0-9_-]{1,64}$/.test(name)) return name;
  } catch {
    // A URI the URL parser cannot read still has a safe default database.
  }
  return "mycord";
}

export function checkRegister(body) {
  const email = String(body.email ?? "").trim().toLowerCase();
  const username = String(body.username ?? "").trim();
  const password = String(body.password ?? "");
  const day = String(body.day ?? "").trim();
  const month = String(body.month ?? "").trim();
  const year = String(body.year ?? "").trim();
  if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email) || email.length > 254) {
    return { error: "Enter a valid email." };
  }
  if (!/^[A-Za-z0-9_-]{2,32}$/.test(username)) {
    return { error: "Username needs 2 to 32 letters, numbers, or underscores." };
  }
  if (password.length < 8 || password.length > 200) {
    return { error: "Password needs at least 8 characters." };
  }
  const dayNumber = Number(day);
  const yearNumber = Number(year);
  if (!MONTHS.includes(month) || !Number.isInteger(dayNumber) || dayNumber < 1 || dayNumber > 31) {
    return { error: "Enter your date of birth." };
  }
  if (!Number.isInteger(yearNumber) || yearNumber < 1930 || yearNumber > 2012) {
    return { error: "Enter your date of birth." };
  }
  return { email, username, password, dob: { day: String(dayNumber), month, year: String(yearNumber) } };
}

export function checkLogin(body) {
  const login = String(body.login ?? "").trim();
  const password = String(body.password ?? "");
  if (!login || !password) return { error: "Enter your email or username, and your password." };
  if (password.length > 200) return { error: "Email, username, or password is wrong." };
  const email = login.includes("@");
  if (email && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(login.toLowerCase())) {
    return { error: "Email, username, or password is wrong." };
  }
  if (!email && !/^[A-Za-z0-9_-]{2,32}$/.test(login)) {
    return { error: "Email, username, or password is wrong." };
  }
  return { login: email ? login.toLowerCase() : login, email, password };
}

export async function hashPassword(password, salt) {
  const key = await crypto.subtle.importKey("raw", encoder.encode(password), "PBKDF2", false, ["deriveBits"]);
  const bits = await crypto.subtle.deriveBits(
    { name: "PBKDF2", salt, iterations: ITERATIONS, hash: "SHA-256" },
    key,
    256,
  );
  return hex(new Uint8Array(bits));
}

export async function passwordMatches(password, saltHex, expected) {
  const actual = await hashPassword(password, fromHex(saltHex));
  return same(actual, expected);
}

export function newSecret() {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return hex(bytes);
}

export function newSalt() {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return bytes;
}

export async function hashSecret(secret) {
  const digest = await crypto.subtle.digest("SHA-256", encoder.encode(secret));
  return hex(new Uint8Array(digest));
}

function hex(bytes) {
  let text = "";
  for (const byte of bytes) text += byte.toString(16).padStart(2, "0");
  return text;
}

function fromHex(text) {
  const bytes = new Uint8Array(text.length / 2);
  for (let i = 0; i < bytes.length; i += 1) bytes[i] = Number.parseInt(text.slice(i * 2, i * 2 + 2), 16);
  return bytes;
}

function same(left, right) {
  if (left.length !== right.length) return false;
  let diff = 0;
  for (let i = 0; i < left.length; i += 1) diff |= left.charCodeAt(i) ^ right.charCodeAt(i);
  return diff === 0;
}
