import { css, Flex, Text, VStack } from '@devup-ui/react'
import Link from 'next/link'

interface Stat {
  value: string
  unit: string
  label: string
  detail: string
}

const STATS: Stat[] = [
  {
    value: '2.2',
    unit: 'µs',
    label: 'Direct JNI dispatch',
    detail: 'Per round-trip via pooled direct ByteBuffers',
  },
  {
    value: '2.9',
    unit: 'µs',
    label: 'Sync dispatch',
    detail: 'Length-prefixed binary wire, no JSON envelope',
  },
  {
    value: '14.5',
    unit: 'GB/s',
    label: 'Streaming throughput',
    detail: '256 KiB chunks, 3.3× faster than v0.x',
  },
  {
    value: '20',
    unit: '%',
    label: 'Faster sync dispatch',
    detail: 'v0.2 zero-copy decode vs v0.1.1, same wire format',
  },
]

export function Performance() {
  return (
    <Flex
      bg="$containerBackground"
      flexDir="column"
      overflow="hidden"
      px="20px"
      py="$spacingSpacing80"
    >
      <VStack alignSelf="center" gap="40px" maxW="1280px" w="100%">
        <VStack gap="16px">
          <Text color="$title" typography="h3">
            Microsecond dispatch, gigabyte/s streaming
          </Text>
          <Text color="$text" typography="body">
            Vespera embeds your Axum router inside the JVM via JNI — zero TCP,
            zero JSON envelope, raw bytes end-to-end. Numbers below are measured
            through the real JNI boundary on AMD Ryzen 9 9950X, JDK 21.
          </Text>
        </VStack>

        <Flex
          flexDir={['column', null, null, 'row']}
          gap={['$spacingSpacing12', null, null, '$spacingSpacing20']}
          w="100%"
        >
          {STATS.map((stat) => (
            <VStack
              key={stat.label}
              bg="$cardBase"
              borderRadius="$spacingSpacing08"
              flex="1"
              gap={['$spacingSpacing08', null, null, '$spacingSpacing12']}
              minH={['unset', null, null, '180px']}
              px={['$spacingSpacing20', null, null, '$spacingSpacing24']}
              py={['$spacingSpacing16', null, null, '$spacingSpacing24']}
            >
              <Flex alignItems="baseline" gap="$spacingSpacing04">
                <Text color="$vesperaPrimary" typography="displaySm">
                  {stat.value}
                </Text>
                <Text color="$vesperaPrimary" typography="h4">
                  {stat.unit}
                </Text>
              </Flex>
              <Text color="$title" typography="titleB">
                {stat.label}
              </Text>
              <Text color="$textSub" typography="bodySm">
                {stat.detail}
              </Text>
            </VStack>
          ))}
        </Flex>

        <Text color="$caption" typography="caption">
          Latency measured on small GET /health round-trips through the real JNI
          boundary; streaming throughput measured with a 64 MiB payload. Full
          methodology and raw runs in the{' '}
          <Link
            className={css({
              color: '$vesperaPrimary',
              textDecoration: 'underline',
            })}
            href="https://github.com/dev-five-git/vespera/blob/main/libs/vespera-bridge/docs/jni-before-after-2026-06-11.md"
            rel="noopener noreferrer"
            target="_blank"
          >
            JNI benchmark report
          </Link>
          .
        </Text>
      </VStack>
    </Flex>
  )
}
