import { devupApi } from '@devup-api/next-plugin'
import { DevupUI } from '@devup-ui/next-plugin'
import type { NextConfig } from 'next'

const nextConfig: NextConfig = {
  /* config options here */
  output: 'export',
  experimental: {
    optimizePackageImports: ['@devup-ui/reset-css', '@devup-ui/components'],
  },
  reactCompiler: true,
}

export default DevupUI(devupApi(nextConfig))
