import { resetCss } from '@devup-ui/reset-css'
import type { Metadata } from 'next'

resetCss()

export const metadata: Metadata = {
  title: 'Devfive',
  description: 'Devfive',
}

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode
}>) {
  return (
    <html lang="ko" suppressHydrationWarning>
      <body>{children}</body>
    </html>
  )
}
