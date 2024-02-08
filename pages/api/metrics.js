import { PrismaClient } from '@prisma/client';

// Instantiate Prisma Client
const prisma = new PrismaClient();

export default async function handler(req, res) {
  if (req.method !== 'GET') {
    return res.status(405).json({ error: 'Method Not Allowed' });
  }

  try {
    const metrics = await prisma.metrics.findMany({
      orderBy: {
        date: 'desc' // Sort by date in descending order
      },
      take: 1 // Limit the result to one record
    });

    if (metrics.length > 0) {
      res.status(200).json(metrics[0]);
    } else {
      res.status(404).json({ error: 'No metrics found' });
    }
  } catch (error) {
    console.error('Error fetching last metric:', error);
    res.status(500).json({ error: 'Internal Server Error' });
  }
}
