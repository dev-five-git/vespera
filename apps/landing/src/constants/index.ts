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
      ],
    },
  ],
  aboutUs: [],
}
