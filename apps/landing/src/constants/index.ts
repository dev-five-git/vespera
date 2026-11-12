/**
 * Versions quoted in the documentation code samples. MDX code fences cannot
 * interpolate values, so the samples spell them out literally;
 * `src/__tests__/docs-versions.test.ts` fails when any sample drifts from
 * these constants, and `scripts/check-version-alignment.ts` (CI lint job)
 * fails when their `major.minor` matches neither the workspace version nor the
 * release the pending changepacks will produce. Bump them together with a
 * release.
 */
export const VESPERA_VERSION = '0.6'
export const VESPERA_BRIDGE_VERSION = '0.6.0'
export const VESPERA_BRIDGE_PLUGIN_VERSION = '0.6.0'

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
