import { globalCss, ThemeScript } from '@devup-ui/react'
import { resetCss } from '@devup-ui/reset-css'
import type { Metadata } from 'next'

import { Header } from '@/components/header'
import { HeaderProvider } from '@/components/header/header-provider'
import { MobileMenu } from '@/components/mobile-menu'
import { SheetProvider } from '@/components/sheet'

resetCss()

globalCss({
  pre: {
    borderRadius: '10px',
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
  code: {
    fontFamily: 'D2Coding',
    fontSize: ['13px', '15px'],
    fontStyle: 'normal',
    fontWeight: 700,
    lineHeight: '1.5',
    letterSpacing: '-0.03em',
  },
  // body: {
  //   fontFamily: 'Pretendard',
  // },
})

export const metadata: Metadata = {
  title: 'Vespera',
  description: 'Vespera',
}

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode
}>) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <script
          dangerouslySetInnerHTML={{
            __html: `(function(w,d,s,l,i){w[l]=w[l]||[];w[l].push({'gtm.start':
new Date().getTime(),event:'gtm.js'});var f=d.getElementsByTagName(s)[0],
j=d.createElement(s),dl=l!='dataLayer'?'&l='+l:'';j.async=true;j.src=
'https://www.googletagmanager.com/gtm.js?id='+i+dl;f.parentNode.insertBefore(j,f);
})(window,document,'script','dataLayer','GTM-PSRKC4QZ')`,
          }}
        />
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
        <SheetProvider>
          <HeaderProvider>
            <Header />
            <MobileMenu />
            {children}
          </HeaderProvider>
        </SheetProvider>
      </body>
    </html>
  )
}
