export const metadata = { title: "next.js on finite" };

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body style={{ fontFamily: "system-ui", background: "#0b0b0f", color: "#e8e8ee", display: "flex", minHeight: "100vh", alignItems: "center", justifyContent: "center" }}>
        {children}
      </body>
    </html>
  );
}
