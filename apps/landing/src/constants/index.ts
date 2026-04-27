export interface SideMenuItem {
  label: string
  value: string
  children?: SideMenuItem[]
}

export const SIDE_MENU_ITEMS: Record<string, SideMenuItem[]> = {
  documentation: [
    {
      label: '개요',
      value: 'overview',
    },
    { label: '설치', value: 'installation' },
    {
      label: '개념',
      value: 'concept',
      children: [
        { label: '개념 1', value: 'concept-1' },
        { label: '개념 2', value: 'concept-2' },
        { label: '개념 3', value: 'concept-3' },
      ],
    },
    { label: '특징', value: 'features' },
    {
      label: 'API',
      value: 'api',
      children: [
        { label: 'API 1', value: 'api-1' },
        { label: 'API 2', value: 'api-2' },
        { label: 'API 3', value: 'api-3' },
      ],
    },
    {
      label: '테마',
      value: 'theme',
      children: [
        { label: '테마 1', value: 'theme-1' },
        { label: '테마 2', value: 'theme-2' },
        { label: '테마 3', value: 'theme-3' },
      ],
    },
  ],
  aboutUs: [],
}
