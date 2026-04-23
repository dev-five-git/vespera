import { Box, Flex, Text, VStack } from '@devup-ui/react'

import { SideMenu } from '@/components/side-menu'
import { SideMenuProvider } from '@/components/side-menu/side-menu-provider'
import { TableOfContentsProvider } from '@/components/table-of-contents'
import {
  TableOfContentsAnchor,
  TableOfContentsIterator,
} from '@/components/table-of-contents/iterator'
import { SIDE_MENU_ITEMS } from '@/constants'

export default function PageLayout({
  children,
}: {
  children: React.ReactNode
}) {
  return (
    <Flex maxW="1440px" minH="calc(100vh - 212px)" mx="auto" pt="68px" w="100%">
      <Box
        borderRight="solid 1px $border"
        display={['none', null, null, 'block']}
      >
        <SideMenuProvider>
          <VStack
            alignItems="center"
            gap="$spacingSpacing04"
            pos="sticky"
            px="$spacingSpacing16"
            py="28px"
            top="68px"
            w="250px"
          >
            <VStack alignItems="center" gap="12px" w="100%">
              <VStack w="100%">
                {SIDE_MENU_ITEMS.documentation.map(
                  ({ value, label, children }) => (
                    <SideMenu key={value} childMenus={children} value={value}>
                      {label}
                    </SideMenu>
                  ),
                )}
              </VStack>
            </VStack>
          </VStack>
        </SideMenuProvider>
      </Box>
      <TableOfContentsProvider>
        <Box
          className="markdown-body"
          flex="1"
          overflowX="auto"
          px={['20px', null, '$spacingSpacing48']}
          py={['20px', null, '$spacingSpacing32']}
        >
          {children}
        </Box>
        <Box display={['none', null, null, 'block']}>
          <VStack
            gap="16px"
            overflow="hidden"
            pos="sticky"
            px="$spacingSpacing16"
            py="28px"
            top="68px"
            w="180px"
          >
            <VStack borderBottom="solid 1px $border" pb="$spacingSpacing08">
              <Flex alignItems="center" py="6px">
                <Text color="$text" flex="1" typography="captionB">
                  Contents
                </Text>
                {/* <MenuIcon Property1="bottom" /> */}
              </Flex>
              <VStack>
                <TableOfContentsIterator>
                  <TableOfContentsAnchor />
                </TableOfContentsIterator>
              </VStack>
            </VStack>
            {/* <Edit property1="default" /> */}
          </VStack>
        </Box>
      </TableOfContentsProvider>
    </Flex>
  )
}
