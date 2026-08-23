import { devupApi } from '@devup-api/next-plugin'
import { DevupUI } from '@devup-ui/next-plugin'
import createMDX from '@next/mdx'
import type { NextConfig } from 'next'

const withMDX = createMDX({
  extension: /\.mdx?$/,
  options: {
    // remark-gfm enables GitHub-flavored markdown (pipe tables, strikethrough,
    // task lists) — mdx-components.tsx already styles table/th/td elements.
    remarkPlugins: ['remark-gfm'],
    rehypePlugins: ['rehype-slug', 'rehype-pretty-code'],
  },
})

const nextConfig: NextConfig = {
  /* config options here */
  pageExtensions: ['js', 'jsx', 'md', 'mdx', 'ts', 'tsx'],
  output: 'export',
  experimental: {
    optimizePackageImports: ['@devup-ui/reset-css', '@devup-ui/components'],
    // TypeScript 7 (the native port) no longer ships the JS compiler API that
    // Next.js drives in-process for type checking. Running `tsc` through its
    // CLI instead is the supported path and keeps us on TS 7.
    useTypeScriptCli: true,
  },
  reactCompiler: true,
}

export default DevupUI(devupApi(withMDX(nextConfig)))
