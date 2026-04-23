'use client'
import { Box, Flex, Grid, Text, VStack } from '@devup-ui/react'
import Link from 'next/link'
import { useState } from 'react'

import { SideMenuItem } from '@/constants'

import { useSideMenu } from './side-menu-provider'

export function SideMenuIcon({ expanded = false }: { expanded?: boolean }) {
  return (
    <Box
      bg="$title"
      boxSize="16px"
      maskImage="url('/icons/chevron.svg')"
      maskPos="center"
      maskRepeat="no-repeat"
      maskSize="contain"
      transform={expanded ? 'rotate(90deg)' : 'rotate(0deg)'}
      transition="transform 0.2s ease-in-out"
    />
  )
}

export function SideMenu({
  hrefPrefix = '/documentation',
  value,
  childMenus,
  children,
}: {
  hrefPrefix?: string
  value?: string
  childMenus?: SideMenuItem[]
  children?: React.ReactNode
}) {
  const [expanded, setExpanded] = useState(false)
  const { selected, setSelected } = useSideMenu()
  const isSelected = selected === value
  const href = [hrefPrefix, value].join('/')
  return (
    <>
      <Link href={!childMenus ? href : '#'}>
        <Flex
          _active={{
            bg: '$vesperaActive',
          }}
          _hover={{
            bg: '$vesperaHover',
          }}
          alignItems="center"
          aria-expanded={childMenus ? expanded : undefined}
          aria-label="side menu"
          bg={isSelected ? '$vesperaSelected' : 'transparent'}
          borderRadius="$spacingSpacing08"
          gap="$spacingSpacing08"
          onClick={() => {
            setSelected(value ?? null)
            setExpanded((prev) => !prev)
          }}
          pl="$spacingSpacing16"
          pr="$spacingSpacing12"
          py={['$spacingSpacing12', null, null, null, '$spacingSpacing08']}
        >
          <Text
            _active={{
              color: '$title',
            }}
            _hover={{
              color: '$title',
            }}
            color={isSelected ? '$title' : '$textSub'}
            flex="1"
            typography="buttonSm"
          >
            {children}
          </Text>
          {childMenus && <SideMenuIcon expanded={expanded} />}
        </Flex>
      </Link>

      {childMenus && (
        <Grid
          gridTemplateRows={expanded ? '1fr' : '0fr'}
          transition="grid-template-rows 0.2s ease-in-out"
        >
          <Box overflow="hidden">
            <Flex gap="2px">
              <Box borderRight="solid 1px $border" w="16px" />
              <VStack flexGrow="1">
                {childMenus.map(({ value, label, children: childMenus }) => (
                  <SideMenu
                    key={value}
                    childMenus={childMenus}
                    hrefPrefix={href}
                    value={value}
                  >
                    {label}
                  </SideMenu>
                ))}
              </VStack>
            </Flex>
          </Box>
        </Grid>
      )}
    </>
  )
}
