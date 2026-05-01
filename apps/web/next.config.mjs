/** @type {import('next').NextConfig} */
const envOrigins = process.env.NEXT_ALLOWED_DEV_ORIGINS?.split(",")
  .map((v) => v.trim())
  .filter(Boolean);

// const nextConfig = {
//   allowedDevOrigins: envOrigins && envOrigins.length > 0 ? envOrigins : ["localhost", "127.0.0.1"]
// };

const nextConfig = {
  allowedDevOrigins: [
    "0087-2401-4900-8855-74cb-f5cc-aae7-e559-3e9a.ngrok-free.app",
    "localhost",
    "127.0.0.1",
  ],
};

export default nextConfig;
