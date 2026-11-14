/**
 * Versions quoted in the documentation code samples. MDX code fences cannot
 * interpolate values, so the samples spell them out literally;
 * `src/__tests__/docs-versions.test.ts` fails when any sample drifts from
 * these constants, and `scripts/check-version-alignment.ts` (CI lint job)
 * fails when their `major.minor` matches neither the workspace version nor the
 * release the pending changepacks will produce. The release job bumps them
 * (and the samples) on the version PR via `scripts/sync-landing-versions.ts`.
 */
export const VESPERA_VERSION = '0.8'
export const VESPERA_BRIDGE_VERSION = '0.8.0'
export const VESPERA_BRIDGE_PLUGIN_VERSION = '0.8.0'

/**
 * Every version literal the documentation samples quote (capture group 1),
 * paired with the constant it must equal. Shared by `docs-versions.test.ts`
 * and `scripts/sync-landing-versions.ts`, which rewrites them on the version
 * PR.
 */
export const DOC_VERSION_PATTERNS: [
  RegExp,
  (
    | 'VESPERA_VERSION'
    | 'VESPERA_BRIDGE_VERSION'
    | 'VESPERA_BRIDGE_PLUGIN_VERSION'
  ),
][] = [
  [/^vespera = "([^"]+)"/gm, 'VESPERA_VERSION'],
  [/^vespera = \{ version = "([^"]+)"/gm, 'VESPERA_VERSION'],
  [/kr\.devfive:vespera-bridge:([0-9][^"]*)"/g, 'VESPERA_BRIDGE_VERSION'],
  [
    /<artifactId>vespera-bridge<\/artifactId>\s*<version>([^<]+)<\/version>/g,
    'VESPERA_BRIDGE_VERSION',
  ],
  [/bridgeVersion\.set\("([^"]+)"\)/g, 'VESPERA_BRIDGE_VERSION'],
  [
    /id\("kr\.devfive\.vespera-bridge"\) version "([^"]+)"/g,
    'VESPERA_BRIDGE_PLUGIN_VERSION',
  ],
]

export interface SideMenuItem {
  label: string
  value: string
  children?: SideMenuItem[]
}

export const SIDE_MENU_ITEMS: Record<string, SideMenuItem[]> = {
  documentation: [
    {
      label: 'Overview',
      value: 'overview',
    },
    { label: 'Installation', value: 'installation' },
    {
      label: 'Core Concepts',
      value: 'concept',
      children: [
        { label: 'File-Based Routing', value: 'concept-1' },
        { label: 'Schema & OpenAPI', value: 'concept-2' },
        { label: 'Validated & 422', value: 'concept-3' },
      ],
    },
    { label: 'Features', value: 'features' },
    { label: 'Migrating to 0.6', value: 'migration' },
    {
      label: 'API Reference',
      value: 'api',
      children: [
        { label: 'vespera! Macro', value: 'api-1' },
        { label: 'Route & Extractors', value: 'api-2' },
        { label: 'schema_type! & More', value: 'api-3' },
      ],
    },
    {
      label: 'JNI / Java',
      value: 'theme',
      children: [
        { label: 'jni_app! & VesperaBridge', value: 'theme-1' },
        { label: 'Dispatch Modes & Wire', value: 'theme-2' },
        { label: 'Streaming & Multi-App', value: 'theme-3' },
        { label: 'Spring Configuration', value: 'theme-4' },
      ],
    },
  ],
  aboutUs: [],
}
