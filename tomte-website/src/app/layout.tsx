import type { Metadata, Viewport } from "next";
import { Bricolage_Grotesque, Hanken_Grotesk, JetBrains_Mono } from "next/font/google";
import "./globals.css";
import { site } from "@/lib/content";
import { Nav } from "@/components/Nav";
import { Footer } from "@/components/Footer";

const bricolage = Bricolage_Grotesque({
  subsets: ["latin"],
  weight: ["400", "600", "700", "800"],
  variable: "--font-bricolage",
  display: "swap",
});

const hanken = Hanken_Grotesk({
  subsets: ["latin"],
  weight: ["400", "500", "600", "700"],
  variable: "--font-hanken",
  display: "swap",
});

const jetbrainsMono = JetBrains_Mono({
  subsets: ["latin"],
  weight: ["400", "500", "700"],
  variable: "--font-jetbrains",
  display: "swap",
});

export const metadata: Metadata = {
  title: {
    default: "tomte: a calm, multi-model coding agent for your terminal",
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
    "multi-model coding agent",
    "OpenAI",
    "Anthropic",
    "provider-agnostic",
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
  themeColor: "#f4f4f1",
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
      className={`${bricolage.variable} ${hanken.variable} ${jetbrainsMono.variable}`}
    >
      <body className="flex min-h-[100dvh] flex-col">
        <Nav />
        <main className="flex-1">{children}</main>
        <Footer />
      </body>
    </html>
  );
}
