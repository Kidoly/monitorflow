import SideBar from '../components/sidebar';
import RamUsage from '../components/ram-usage';

export default function Home() {
  return (
    <div className="flex min-h-screen bg-primary">
      <SideBar />
      <main className="flex flex-1 flex-col items-center justify-between p-24">
        <section>
          <RamUsage />
        </section>
      </main>
    </div>
  );
}