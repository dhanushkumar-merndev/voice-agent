"use client";

import { Suspense } from "react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { BookOpenIcon, BotIcon, LayoutDashboardIcon, LogOutIcon, PhoneCallIcon, PhoneOutgoingIcon } from "lucide-react";

import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarRail,
} from "@/components/ui/sidebar";

const NAV = [
  { href: "/", label: "Dashboard", icon: LayoutDashboardIcon },
  { href: "/calls", label: "Calls", icon: PhoneCallIcon },
  { href: "/agents", label: "Agents", icon: BotIcon },
  { href: "/knowledge", label: "Knowledge bases", icon: BookOpenIcon },
];

/** The nav links; the current page is read at request time, so it streams in. */
function NavItems({ pathname }: { pathname?: string }) {
  const active = (href: string) => pathname !== undefined && (href === "/" ? pathname === "/" : pathname.startsWith(href));
  return NAV.map((item) => (
    <SidebarMenuItem key={item.href}>
      <SidebarMenuButton asChild isActive={active(item.href)} tooltip={item.label}>
        <Link href={item.href}>
          <item.icon />
          <span>{item.label}</span>
        </Link>
      </SidebarMenuButton>
    </SidebarMenuItem>
  ));
}

function ActiveNavItems() {
  return <NavItems pathname={usePathname()} />;
}

export function AppSidebar() {
  const router = useRouter();

  async function logout() {
    await fetch("/api/auth/logout", { method: "POST" });
    router.replace("/login");
    router.refresh();
  }

  return (
    <Sidebar collapsible="icon">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg" asChild>
              <Link href="/">
                <div className="flex size-8 items-center justify-center rounded-lg bg-primary text-primary-foreground">
                  <PhoneOutgoingIcon className="size-4" />
                </div>
                <div className="grid leading-tight">
                  <span className="font-semibold">Voice Agents</span>
                  <span className="text-xs text-muted-foreground">Plivo · Sarvam</span>
                </div>
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupContent>
            <SidebarMenu>
              <Suspense fallback={<NavItems />}>
                <ActiveNavItems />
              </Suspense>
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>
      <SidebarFooter>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton onClick={logout} tooltip="Sign out">
              <LogOutIcon />
              <span>Sign out</span>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
  );
}
