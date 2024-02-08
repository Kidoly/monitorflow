import SideBar from '../components/sidebar';
import { useState, useEffect } from 'react';
import '../app/globals.css';

export default function Servers() {
  const [lastMetric, setLastMetric] = useState(null);

  useEffect(() => {
    async function fetchLastMetric() {
      try {
        const response = await fetch('/api/metrics');
        if (!response.ok) {
          throw new Error('Failed to fetch last metric');
        }
        const data = await response.json();
        setLastMetric(data);
      } catch (error) {
        console.error('Error fetching last metric:', error);
      }
    }

    fetchLastMetric();
  }, []);

  return (
    <div className="flex min-h-screen bg-primary">
      <SideBar />
      <main className="flex flex-1 flex-col items-center justify-between p-24">
        {lastMetric && (
          <section>
            <p>Last Metric:</p>
            <pre>{JSON.stringify(lastMetric, null, 2)}</pre>
          </section>
        )}
      </main>
    </div>
  );
}
