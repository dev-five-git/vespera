import { Box, Center, css, Flex, Text, VStack } from '@devup-ui/react'
import { Image } from '@devup-ui/react'
import type { Metadata } from 'next'
import Link from 'next/link'

import {
  ExampleContainer,
  ExampleImage,
  ExampleProvider,
} from '@/components/app/example'
import { Button } from '@/components/button'
import { GnbIcon } from '@/components/header/gnb-icon'
import { HeaderSentinel } from '@/components/header/header-sentinel'
import { Performance } from '@/components/performance'

export const metadata: Metadata = {
  alternates: {
    canonical: '/',
  },
}

const EXAMPLES = [
  {
    id: '1',
    title: '1. Drop in a route',
    description:
      'Write a pub async fn in src/routes/ with #[vespera::route]. The file path becomes the URL — no router wiring, no manual registration.',
    imageUrl: '/images/rust-code.png',
  },
  {
    id: '2',
    title: '2. Serve with one macro',
    description:
      'vespera!() discovers every route and cron job at compile time and generates your OpenAPI 3.1 spec. Chain .serve(addr) and Swagger UI is live at /docs.',
    imageUrl: '/images/hero.webp',
  },
  {
    id: '3',
    title: '3. Embed in Spring — optional',
    description:
      'Add vespera::jni_app! and call VesperaBridge.init() from Java. The same router runs inside the JVM over a binary wire — microsecond round-trips, no TCP.',
    imageUrl: '/images/join-us-bg.webp',
  },
]

export default function HomePage() {
  return (
    <>
      <Box bg="#0A0E1A" color="#FFF" minH="100vh">
        <Center
          bg="url(/images/hero.webp) center/cover no-repeat"
          flexDir="column"
          h="1080px"
          pb="60px"
          pos="relative"
          pt="128px"
          px="40px"
        >
          <VStack
            alignItems="center"
            gap="$spacingSpacing64"
            maxW="1280px"
            w="100%"
          >
            <VStack alignItems="center" gap="$spacingSpacing32" w="100%">
              <Text color="$title" textAlign="center" typography="displaySm">
                The fastest way to ship <br />
                documented Rust APIs.
              </Text>
              <Text color="$title" textAlign="center" typography="title">
                Vespera turns plain Axum handlers into a typed, validated API
                with OpenAPI 3.1 generated at compile time. <br />
                File-based routing, automatic Swagger UI, and a binary JNI
                bridge that embeds your router <br />
                inside Spring Boot with microsecond round-trips.
              </Text>
            </VStack>
            <Link href="/documentation/installation">
              <Button>Get started</Button>
            </Link>
          </VStack>
        </Center>

        <Center
          bg="$containerBackground"
          flexDir="column"
          overflow="hidden"
          px="20px"
          py="$spacingSpacing80"
        >
          <VStack gap="40px" maxW="1280px" w="100%">
            <VStack gap="16px">
              <Text color="$title" typography="h3">
                FastAPI-grade DX, Rust-grade performance
              </Text>
              <Text color="$text" typography="body">
                Vespera turns your Axum routes into a typed, validated, embeddable API
                with one macro. File-based routing, compile-time OpenAPI 3.1, and a
                JNI bridge that lets Spring host your Rust router with microsecond
                round-trips — no TCP, no JSON envelope.
              </Text>
            </VStack>
            <VStack flexDir={[null, null, null, 'row']} gap={5}>
              {[
                {
                  title: 'Zero-config OpenAPI 3.1',
                  description:
                    'Drop handlers into src/routes/, derive Schema on your types, and Vespera generates the full OpenAPI 3.1 spec at compile time. No annotations, no runtime registration, no hand-written JSON.',
                },
                {
                  title: 'Type-safe validation',
                  description:
                    'Wrap any extractor in Validated<T> and garde runs before your handler. Failures become a structured 422 response automatically — under JNI, errors are hoisted into the wire header so Java decoders never special-case error shapes.',
                },
                {
                  title: 'Embed Rust in Spring',
                  description:
                    'JNI in-process dispatch with a length-prefixed binary wire format. Multipart, PDFs, and images travel as raw bytes — no TCP socket, no JSON envelope, no base64 — the same Axum routes Spring users hit directly.',
                },
                {
                  title: 'Microsecond dispatch',
                  description:
                    'Sync round-trip in ~2.9 µs, direct ByteBuffer path in ~2.2 µs, streaming throughput up to 14.5 GB/s — measured end-to-end across the real JNI boundary, not just on the Rust side.',
                },
              ].map(({ title, description }) => (
                <Flex
                  key={title}
                  bg="$cardBase"
                  borderRadius="$spacingSpacing08"
                  minH={['200px', null, null, '320px']}
                  overflow="hidden"
                  px={['$spacingSpacing20', null, null, '$spacingSpacing24']}
                  py={['$spacingSpacing16', null, null, '$spacingSpacing24']}
                >
                  <VStack
                    flex="1"
                    gap={['10px', null, null, '$spacingSpacing12']}
                  >
                    <Text color="$title" typography="title">
                      {title}
                    </Text>
                    <Text color="$textSub" typography="body">
                      {description}
                    </Text>
                  </VStack>
                </Flex>
              ))}
            </VStack>
          </VStack>
        </Center>

        <Performance />

        <ExampleProvider defaultSelected={EXAMPLES[0].id} examples={EXAMPLES}>
          <HeaderSentinel
            className={css({
              display: 'flex',
              justifyContent: 'center',
              alignItems: 'center',
              bg: '#10131F',
              flexDir: 'column',
              overflow: 'hidden',
              px: '20px',
              py: ['80px', null, null, '120px'],
            })}
          >
            <VStack gap="40px" maxW={[null, null, null, '1280px']} w="100%">
              <VStack gap="16px">
                <Text color="#FFF" typography="h3">
                  Zero to documented API in three steps
                </Text>
                <Text color="#FFF" typography="body">
                  No boilerplate, no YAML, no hand-written specs — the macro
                  does the wiring, you write handlers.{' '}
                </Text>
              </VStack>
              <VStack
                alignItems="center"
                flexDir={[null, null, null, 'row-reverse']}
                gap="$spacingSpacing32"
                pos="relative"
              >
                <Flex
                  bg="linear-gradient(90deg, #161A2A 0%, #121F33 100%)"
                  borderRadius="$spacingSpacing08"
                  flexShrink="0"
                  h={['320px', null, null, '424px']}
                  justifyContent="center"
                  overflow="hidden"
                  pos="relative"
                  px="$spacingSpacing20"
                  py="20px"
                  w={['100%', null, null, '624px']}
                >
                  <ExampleImage />
                  <Box
                    bottom="27px"
                    left="50%"
                    pos="absolute"
                    transform="translateX(-50%)"
                  >
                    <Link href="/documentation">
                      <Button>Learn more</Button>
                    </Link>
                  </Box>
                </Flex>
                <VStack gap="$spacingSpacing12" w="100%">
                  {EXAMPLES.map(({ id, title, description }) => (
                    <ExampleContainer key={id} value={id}>
                      <VStack
                        flex="1"
                        gap={['10px', null, null, '$spacingSpacing12']}
                      >
                        <Text color="#FFF" typography="title">
                          {title}
                        </Text>
                        <Text color="#FFF" typography="body">
                          {description}
                        </Text>
                      </VStack>
                    </ExampleContainer>
                  ))}
                </VStack>
              </VStack>
            </VStack>
          </HeaderSentinel>
        </ExampleProvider>

        <HeaderSentinel
          className={css({
            alignItems: 'center',
            display: 'flex',
            flexDir: 'column',
            bg: '#000',
            gap: '40px',
            overflow: 'hidden',
            pos: 'relative',
            px: ['20px', null, null, '40px'],
            py: ['80px', null, null, '120px'],
            h: ['600px', null, null, 'unset'],
          })}
        >
          <VStack
            flexDir={[null, null, null, 'row']}
            h={['100%', null, null, 'unset']}
            justifyContent={[null, null, null, 'flex-end']}
            maxW="1280px"
            pos="relative"
            w="100%"
          >
            <VStack
              gap="40px"
              justifyContent="center"
              maxW="480px"
              w="100%"
              zIndex="10"
            >
              <VStack gap="16px">
                <Text color="#FFF" typography="h3">
                  Join our community
                </Text>
                <Text color="#FFF" typography="body">
                  Join our Discord to talk Rust APIs, JNI embedding, and what
                  Vespera should build next.{' '}
                </Text>
              </VStack>
              <Flex alignItems="center" gap="16px">
                <Link
                  href="https://discord.com/invite/8zjcGc7cWh"
                  rel="noopener noreferrer"
                  target="_blank"
                >
                  <Flex
                    _active={{
                      bg: '#6B9FFF99',
                    }}
                    _hover={{
                      bg: '#6B9FFF66',
                    }}
                    alignItems="center"
                    bg="#6B9FFF40"
                    borderRadius="100px"
                    cursor="pointer"
                    p="16px"
                  >
                    <GnbIcon
                      className={css({ bg: '$vesperaPrimary' })}
                      icon="discord"
                    />
                  </Flex>
                </Link>
                <Link
                  href="https://open.kakao.com/o/giONwVAh"
                  rel="noopener noreferrer"
                  target="_blank"
                >
                  <Flex
                    _active={{
                      bg: '#6B9FFF99',
                    }}
                    _hover={{
                      bg: '#6B9FFF66',
                    }}
                    alignItems="center"
                    bg="#6B9FFF40"
                    borderRadius="100px"
                    p="16px"
                  >
                    <GnbIcon
                      className={css({ bg: '$vesperaPrimary' })}
                      icon="kakao"
                    />
                  </Flex>
                </Link>
                <Link
                  href="https://devfive.kr"
                  rel="noopener noreferrer"
                  target="_blank"
                >
                  <Flex
                    _active={{
                      bg: '#6B9FFF99',
                    }}
                    _hover={{
                      bg: '#6B9FFF66',
                    }}
                    alignItems="center"
                    bg="#6B9FFF40"
                    borderRadius="100px"
                    p="16px"
                  >
                    <GnbIcon
                      className={css({ bg: '$vesperaPrimary' })}
                      icon="devfive"
                    />
                  </Flex>
                </Link>
              </Flex>
            </VStack>
            <Image
              alt="join our community background image"
              bottom="-305px"
              boxSize="500px"
              left="-142px"
              pos="absolute"
              src="/images/join-us-bg.webp"
            />
          </VStack>
        </HeaderSentinel>
      </Box>
    </>
  )
}
