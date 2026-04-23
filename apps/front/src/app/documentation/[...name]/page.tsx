import { notFound } from 'next/navigation'

import { SIDE_MENU_ITEMS, SideMenuItem } from '@/constants'

function getPageNamesFromSideMenuItems(items: SideMenuItem[]): string[] {
  function joinNames(item: SideMenuItem, prefix: string = ''): string[] {
    const name = [...(prefix ? [prefix] : []), item.value].join('.')
    return [
      name,
      ...(item.children?.flatMap((child) => joinNames(child, name)) ?? []),
    ]
  }

  return items.flatMap((item) => joinNames(item))
}

export const dynamicParams = false

export function generateStaticParams() {
  const names = getPageNamesFromSideMenuItems(SIDE_MENU_ITEMS.documentation)
  return names.map((name) => ({ name: name.split('.') }))
}

export default async function Page({
  params,
}: {
  params: Promise<{ name: string[] }>
}) {
  const { name } = await params
  const names = getPageNamesFromSideMenuItems(SIDE_MENU_ITEMS.documentation)
  if (!names.includes(name.join('.'))) notFound()
  const { default: Documentation } = await import(`./${name.join('.')}.mdx`)
  return <Documentation />
}
