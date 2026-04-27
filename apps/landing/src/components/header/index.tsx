import { Center, css, Flex, Image } from '@devup-ui/react'
import Link from 'next/link'

import { PathnameBoundary } from '../pathname-boundary'
import { Search } from '../search'
import { SearchForm } from '../search/form'
import { SheetRouteBoundary, SheetRouteTrigger } from '../sheet/router'
import { LightThemeBoundary } from '../theme/light-theme-boundary'
import { ThemeToggle } from '../theme/theme-toggle'
import { Effect } from './effect'
import { GnbIcon } from './gnb-icon'
import { HeaderContainer } from './header-container'
import { HeaderGnbIcon } from './header-gnb-icon'
import { HeaderHamburger } from './header-hamburger'
import { HeaderLogoText } from './header-logo-text'
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
          <Link href="/">
            <Flex alignItems="center" gap="8px">
              <Image
                alt="Vespera logo"
                h="28px"
                src="/icons/logo-image.svg"
                w="21px"
              />
              <HeaderLogoText />
            </Flex>
          </Link>

          <Flex alignItems="center" display={['none', null, null, 'flex']}>
            <Link href="/documentation">
              <Menu value="/documentation">Documentation</Menu>
            </Link>
            <Link href="/about-us">
              <Menu value="/about-us">About us</Menu>
            </Link>
          </Flex>
        </Center>
        <Flex alignItems="center" gap="$spacingSpacing24">
          <Flex alignItems="center" display={['flex', null, null, 'none']}>
            <SheetRouteBoundary name="mobile-menu" reverse>
              <SheetRouteTrigger name="search">
                <Effect className={css({ _hover: { bg: 'revert' } })}>
                  <HeaderGnbIcon icon="search" />
                </Effect>
              </SheetRouteTrigger>
            </SheetRouteBoundary>
            <SheetRouteTrigger name="mobile-menu">
              <Effect className={css({ _hover: { bg: 'revert' } })}>
                <HeaderHamburger />
              </Effect>
            </SheetRouteTrigger>
          </Flex>
          <Flex
            alignItems="center"
            display={['none', null, null, 'flex']}
            gap="$spacingSpacing24"
          >
            <PathnameBoundary candidates={['/']} reverse>
              <Flex alignItems="center" display={['none', null, null, 'flex']}>
                <SearchForm>
                  <Search id="desktop-search" name="search" />
                </SearchForm>
              </Flex>
            </PathnameBoundary>

            <Flex alignItems="center" display={['none', null, null, 'flex']}>
              <Link
                href="https://github.com/dev-five-git/vespera"
                rel="noopener noreferrer"
                target="_blank"
              >
                <Effect>
                  <GnbIcon icon="github" />
                </Effect>
              </Link>
              <Link
                href="https://discord.com/invite/8zjcGc7cWh"
                rel="noopener noreferrer"
                target="_blank"
              >
                <Effect>
                  <GnbIcon icon="discord" />
                </Effect>
              </Link>
              <Link
                href="https://open.kakao.com/o/giONwVAh"
                rel="noopener noreferrer"
                target="_blank"
              >
                <Effect>
                  <GnbIcon icon="kakao" />
                </Effect>
              </Link>
              <LightThemeBoundary>
                <ThemeToggle>
                  <Effect>
                    <GnbIcon icon="theme-light" />
                  </Effect>
                </ThemeToggle>
              </LightThemeBoundary>
              <LightThemeBoundary reverse>
                <ThemeToggle>
                  <Effect>
                    <GnbIcon icon="theme-dark" />
                  </Effect>
                </ThemeToggle>
              </LightThemeBoundary>
            </Flex>
          </Flex>
        </Flex>
      </Flex>
    </HeaderContainer>
  )
}
