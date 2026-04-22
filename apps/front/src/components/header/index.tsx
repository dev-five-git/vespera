import { Box, Center, css, Flex, Image } from '@devup-ui/react'

import { Hamburger } from '../mobile-menu/hamburger'
import { SheetBoundary, SheetTrigger } from '../sheet'
import { Effect } from './effect'
import { GnbIcon } from './gnb-icon'
import { HeaderContainer } from './header-container'
import { Menu } from './menu'

export function Header() {
  return (
    <HeaderContainer>
      <Flex
        alignItems="center"
        justifyContent="space-between"
        maxW="1440px"
        w="100%"
      >
        <Center
          flexDir={[null, null, null, 'row']}
          gap={[null, null, null, '16px']}
        >
          <Flex alignItems="center" gap="8px">
            <Image h="28px" src="/icons/logo-image.svg" w="21px" />
            <Box
              bg="$title"
              h="28px"
              maskImage="url('/icons/logo-text.svg')"
              maskPos="center"
              maskRepeat="no-repeat"
              maskSize="contain"
              w="112px"
            />
          </Flex>

          <Flex alignItems="center" display={['none', null, null, 'flex']}>
            <Menu>Documentation</Menu>
            <Menu>About us</Menu>
          </Flex>
        </Center>
        <Flex alignItems="center" gap="$spacingSpacing24">
          <Flex alignItems="center" display={['flex', null, null, 'none']}>
            <SheetBoundary reverse>
              <Effect className={css({ _hover: { bg: 'revert' } })}>
                <GnbIcon icon="search" />
              </Effect>
            </SheetBoundary>
            <SheetTrigger>
              <Effect className={css({ _hover: { bg: 'revert' } })}>
                <Hamburger />
              </Effect>
            </SheetTrigger>
          </Flex>
          <Flex alignItems="center" display={['none', null, null, 'flex']}>
            <Effect>
              <GnbIcon icon="github" />
            </Effect>
            <Effect>
              <GnbIcon icon="discord" />
            </Effect>
            <Effect>
              <GnbIcon icon="kakao" />
            </Effect>
            <Effect>
              <GnbIcon icon="theme-light" />
            </Effect>
          </Flex>
        </Flex>
      </Flex>
    </HeaderContainer>
  )
}
