import createMDX from "fumadocs-mdx/config";

const withMDX = createMDX();

/** @type {import('next').NextConfig} */
const config = {
  reactStrictMode: true,
  experimental: {
    mdxRs: true,
    useLightningcss: true,
  },
};

export default withMDX(config);
