import { Grid, Text, VStack } from '@devup-ui/react'
import type { Metadata } from 'next'

import { AboutUs } from '@/components/about-us'

export const metadata: Metadata = {
  title: 'Vespera - About us',
  description: 'About the team behind Vespera',
  alternates: {
    canonical: '/about-us',
  },
  openGraph: {
    title: 'Vespera - About us',
    description: 'About the team behind Vespera',
    url: '/about-us',
    siteName: 'Vespera',
  },
}

export default function Page() {
  return (
    <VStack
      flex="1"
      gap="16px"
      maxW="1000px"
      minH="calc(100vh - 212px)"
      mx="auto"
      overflow="hidden"
      px="20px"
      py="100px"
      w="100%"
    >
      <Text color="$caption" typography="captionB">
        About us
      </Text>
      <VStack gap="$spacingSpacing40">
        <VStack gap="8px">
          <Text color="$title" typography="h2">
            Title
          </Text>
          <Text color="$text" typography="body">
            Lorem ipsum dolor sit amet, consectetur adipiscing elit. Nullam
            venenatis, elit in hendrerit porta, augue ante scelerisque diam, ac
            egestas lacus est nec urna. Cras commodo risus hendrerit, suscipit
            nibh at, porttitor dui.
          </Text>
        </VStack>
        <Grid
          gap="$spacingSpacing16"
          gridTemplateColumns={['repeat(1,1fr)', null, null, 'repeat(3, 1fr)']}
          gridTemplateRows="repeat(2, 1fr)"
        >
          <AboutUs />
          <AboutUs />
          <AboutUs />
        </Grid>
      </VStack>
    </VStack>
  )
}
