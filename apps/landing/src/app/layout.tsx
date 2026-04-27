import { globalCss, ThemeScript } from '@devup-ui/react'
import { resetCss } from '@devup-ui/reset-css'
import type { Metadata } from 'next'

import { Footer } from '@/components/footer'
import { Header } from '@/components/header'
import { HeaderProvider } from '@/components/header/header-provider'
import { MobileMenu } from '@/components/mobile-menu'
import { SearchDimmer } from '@/components/search/dimmer'
import {
  SearchContextBoundary,
  SearchProvider,
} from '@/components/search/provider'
import { SearchResult } from '@/components/search/result'
import {
  SheetRoute,
  SheetRouteBoundary,
  SheetRouter,
} from '@/components/sheet/router'

import { SearchSheet } from './documentation/_components/search-sheet'

resetCss()

globalCss({
  html: {
    scrollPaddingTop: '68px',
  },
  body: {
    fontFamily: 'Pretendard',
  },
  figure: {
    margin: 0,
  },
  code: {
    py: '8px',
    px: '16px',
    fontFamily: 'D2Coding',
    fontSize: ['13px', '15px'],
    fontStyle: 'normal',
    fontWeight: 700,
    lineHeight: '1.5',
    letterSpacing: '-0.03em',
  },
  pre: {
    borderRadius: '10px',
  },
  'pre,code,figure': {
    overflowX: 'auto',
  },
  'pre>code': {
    overflowX: 'auto',
  },
  a: {
    textDecoration: 'none',
  },
  fontFaces: [
    {
      fontFamily: 'Pretendard',
      src: 'url(https://cdn.jsdelivr.net/gh/orioncactus/pretendard@v1.3.9/packages/pretendard/dist/web/static/woff2/Pretendard-ExtraBold.woff2) format("woff2")',
      fontWeight: 800,
      fontStyle: 'normal',
    },
    {
      fontFamily: 'Pretendard',
      src: 'url(https://cdn.jsdelivr.net/gh/orioncactus/pretendard@v1.3.9/packages/pretendard/dist/web/static/woff2/Pretendard-Bold.woff2) format("woff2")',
      fontWeight: 700,
      fontStyle: 'normal',
    },
    {
      fontFamily: 'Pretendard',
      src: 'url(https://cdn.jsdelivr.net/gh/orioncactus/pretendard@v1.3.9/packages/pretendard/dist/web/static/woff2/Pretendard-SemiBold.woff2) format("woff2")',
      fontWeight: 600,
      fontStyle: 'normal',
    },
    {
      fontFamily: 'Pretendard',
      src: 'url(https://cdn.jsdelivr.net/gh/orioncactus/pretendard@v1.3.9/packages/pretendard/dist/web/static/woff2/Pretendard-Medium.woff2) format("woff2")',
      fontWeight: 500,
      fontStyle: 'normal',
    },
    {
      fontFamily: 'Pretendard',
      src: 'url(https://cdn.jsdelivr.net/gh/orioncactus/pretendard@v1.3.9/packages/pretendard/dist/web/static/woff2/Pretendard-Regular.woff2) format("woff2")',
      fontWeight: 400,
      fontStyle: 'normal',
    },
    {
      fontFamily: 'Pretendard',
      src: 'url(https://cdn.jsdelivr.net/gh/orioncactus/pretendard@v1.3.9/packages/pretendard/dist/web/static/woff2/Pretendard-Light.woff2) format("woff2")',
      fontWeight: 300,
      fontStyle: 'normal',
    },
    {
      fontFamily: 'Pretendard',
      src: 'url(https://cdn.jsdelivr.net/gh/orioncactus/pretendard@v1.3.9/packages/pretendard/dist/web/static/woff2/Pretendard-Thin.woff2) format("woff2")',
      fontWeight: 100,
      fontStyle: 'normal',
    },
    {
      fontFamily: 'D2Coding',
      src: 'url(https://cdn.jsdelivr.net/gh/projectnoonnu/noonfonts_three@1.0/D2Coding.woff) format("woff")',
      fontWeight: 400,
      fontDisplay: 'swap',
    },
  ],
})

export const metadata: Metadata = {
  title: 'Vespera',
  description: 'FastAPI-like DX for Rust/Axum with automated OpenAPI 3.1',
  alternates: {
    canonical: 'https://vespera.devfive.kr',
  },
  metadataBase: new URL('https://vespera.devfive.kr'),
  openGraph: {
    title: 'Vespera',
    description: 'FastAPI-like DX for Rust/Axum with automated OpenAPI 3.1',
    images: ['https://vespera.devfive.kr/og-image.webp'],
    siteName: 'Vespera',
    type: 'website',
    url: 'https://vespera.devfive.kr',
  },
}

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode
}>) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <link
          as="font"
          crossOrigin="anonymous"
          href="https://cdn.jsdelivr.net/gh/projectnoonnu/noonfonts_three@1.0/D2Coding.woff"
          rel="preload"
          type="font/woff"
        />
        {[
          'ExtraBold',
          'Bold',
          'SemiBold',
          'Medium',
          'Regular',
          'Light',
          'Thin',
        ].map((font) => (
          <link
            key={font}
            as="font"
            crossOrigin="anonymous"
            href={`https://cdn.jsdelivr.net/gh/orioncactus/pretendard@v1.3.9/packages/pretendard/dist/web/static/woff2/Pretendard-${font}.woff2`}
            rel="preload"
            type="font/woff2"
          />
        ))}
        <ThemeScript auto />
        <meta content="width=device-width, initial-scale=1.0" name="viewport" />
        <link href="/favicon.ico" rel="shortcut icon" />
      </head>
      <body>
        <SearchProvider>
          <SheetRouter>
            <SheetRoute name="mobile-menu">
              <SheetRoute name="search">
                <HeaderProvider>
                  <SearchDimmer />
                  <Header />
                  <MobileMenu />
                  <SearchSheet />
                  <SearchContextBoundary state="value">
                    <SearchContextBoundary state="dimmed">
                      <SearchResult />
                    </SearchContextBoundary>
                    <SearchContextBoundary reverse state="dimmed">
                      <SheetRouteBoundary name="search">
                        <SearchResult />
                      </SheetRouteBoundary>
                    </SearchContextBoundary>
                  </SearchContextBoundary>
                  {children}
                  <Footer />
                </HeaderProvider>
              </SheetRoute>
            </SheetRoute>
          </SheetRouter>
        </SearchProvider>
      </body>
    </html>
  )
}
