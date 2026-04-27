import { css, Flex, Text, VStack } from '@devup-ui/react'
import Link from 'next/link'

import { SIDE_MENU_ITEMS } from '@/constants'

import { Effect } from '../header/effect'
import { GnbIcon } from '../header/gnb-icon'
import { SheetRouteContainer } from '../sheet/router'
import { SideMenu } from '../side-menu'
import { SideMenuProvider } from '../side-menu/side-menu-provider'
import { LightThemeBoundary } from '../theme/light-theme-boundary'
import { ThemeToggle } from '../theme/theme-toggle'
import { SideMenuClickDetector } from './side-menu-click-detector'

export function MobileMenu() {
  return (
    <SheetRouteContainer
      className={css({
        borderRadius: '0px',
        top: '68px',
        overflow: 'auto',
        pb: '$spacingSpacing80',
      })}
      name="mobile-menu"
      position="right"
    >
      <SideMenuProvider>
        <SideMenuClickDetector
          className={css({
            alignItems: 'center',
            gap: '$spacingSpacing08',
            w: '100%',
          })}
        >
          <VStack
            alignItems="center"
            gap="12px"
            py="$spacingSpacing08"
            w="100%"
          >
            <VStack px="20px" w="100%">
              <Flex
                alignItems="center"
                borderRadius="$spacingSpacing08"
                py="$spacingSpacing12"
              >
                <Text color="$title" flex="1" typography="buttonSm">
                  Documentation
                </Text>
              </Flex>
              {SIDE_MENU_ITEMS.documentation.map(
                ({ value, label, children }) => (
                  <SideMenu key={value} childMenus={children} value={value}>
                    {label}
                  </SideMenu>
                ),
              )}
            </VStack>
            <Flex
              alignItems="center"
              borderRadius="$spacingSpacing08"
              px="20px"
              py="$spacingSpacing12"
              w="100%"
            >
              <Text color="$title" flex="1" typography="buttonSm">
                About us
              </Text>
            </Flex>
          </VStack>
          <Flex alignItems="center">
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
        </SideMenuClickDetector>
      </SideMenuProvider>
    </SheetRouteContainer>
  )
}
