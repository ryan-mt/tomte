import type { Metadata, Viewport } from "next";
import { Fraunces, Spline_Sans, IBM_Plex_Mono } from "next/font/google";
import "./globals.css";
import { site } from "@/lib/content";
import { Nav } from "@/components/Nav";
import { Footer } from "@/components/Footer";

const fraunces = Fraunces({
  subsets: ["latin"],
  variable: "--font-fraunces",
  display: "swap",
  axes: ["opsz", "SOFT", "WONK"],
});

const spline = Spline_Sans({
  subsets: ["latin"],
  weight: ["400", "500", "600", "700"],
  variable: "--font-spline",
  display: "swap",
});

const plexMono = IBM_Plex_Mono({
  subsets: ["latin"],
  weight: ["400", "500", "600"],
  variable: "--font-plexmono",
  display: "swap",
});

export const metadata: Metadata = {
  title: {
    default: "tomte: the coding agent that proves its work",
    template: "%s · tomte",
  },
  description: site.description,
  applicationName: "tomte",
  authors: [{ name: "Ryan" }],
  keywords: [
    "tomte",
    "coding agent",
    "terminal",
    "Rust CLI",
    "verified",
    "proof capsule",
    "multi-model coding agent",
    "OpenAI",
    "Anthropic",
    "developer tools",
  ],
  openGraph: {
    title: "tomte",
    description: site.description,
    type: "website",
    siteName: "tomte",
  },
  twitter: {
    card: "summary_large_image",
    title: "tomte",
    description: site.description,
  },
};

export const viewport: Viewport = {
  themeColor: "#0c1216",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html
      lang="en"
      suppressHydrationWarning
      className={`${fraunces.variable} ${spline.variable} ${plexMono.variable}`}
    >
      <body className="flex min-h-[100dvh] flex-col">
        <Nav />
        <main className="flex-1">{children}</main>
        <Footer />
      </body>
    </html>
  );
}
