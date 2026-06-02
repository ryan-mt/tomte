export function PageHeader({
  kicker,
  title,
  intro,
}: {
  kicker?: string;
  title: string;
  intro: string;
}) {
  return (
    <header className="border-b border-line">
      <div className="mx-auto max-w-[1200px] px-5 pb-14 pt-16 sm:px-8 sm:pb-16 sm:pt-20">
        {kicker ? <span className="mono-label">{kicker}</span> : null}
        <h1 className="mt-4 max-w-4xl font-display text-[2.9rem] font-extrabold leading-[0.94] tracking-[-0.03em] text-ink sm:text-[4rem]">
          {title}
        </h1>
        <p className="mt-6 max-w-2xl text-[1.125rem] leading-relaxed text-ink-2">
          {intro}
        </p>
      </div>
    </header>
  );
}
