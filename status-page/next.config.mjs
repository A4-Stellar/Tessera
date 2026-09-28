/** @type {import('next').NextConfig} */
const nextConfig = {
  // Status page is standalone — no rewrites or basePath needed
  output: "standalone",
  // Prevent the status page from trying to import the docs or API packages
  experimental: {
    optimizePackageImports: ["recharts"],
  },
};

export default nextConfig;
