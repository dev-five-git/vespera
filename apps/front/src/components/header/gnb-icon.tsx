import { Box } from '@devup-ui/react'
import { ComponentProps } from 'react'

export interface GnbIconProps {
  icon: 'discord' | 'github' | 'kakao' | 'theme-light' | 'theme-dark'
}

export function GnbIcon({
  icon,
  ...props
}: GnbIconProps & ComponentProps<typeof Box<'div'>>) {
  return (
    <Box
      bg="$title"
      boxSize="24px"
      maskImage={
        {
          'theme-light': "url('/icons/theme-light.svg')",
          'theme-dark': "url('/icons/theme-dark.svg')",
          discord: "url('/icons/discord.svg')",
          github: "url('/icons/github.svg')",
          kakao: "url('/icons/kakao.svg')",
        }[icon]
      }
      maskPos="center"
      maskRepeat="no-repeat"
      maskSize="contain"
      styleOrder={1}
      {...props}
    />
  )
}
