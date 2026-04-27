import { devupApi } from '@devup-api/next-plugin'
import { DevupUI } from '@devup-ui/next-plugin'
import createMDX from '@next/mdx'
import type { NextConfig } from 'next'

const withMDX = createMDX({
  extension: /\.mdx?$/,
  options: {
    rehypePlugins: ['rehype-slug', 'rehype-pretty-code'],
  },
})

const nextConfig: NextConfig = {
  /* config options here */
  pageExtensions: ['js', 'jsx', 'md', 'mdx', 'ts', 'tsx'],
  output: 'export',
  experimental: {
    optimizePackageImports: ['@devup-ui/reset-css', '@devup-ui/components'],
  },
  reactCompiler: true,
}

export default DevupUI(devupApi(withMDX(nextConfig)))
