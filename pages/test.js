const { PrismaClient } = require('@prisma/client');

const prisma = new PrismaClient();

async function getLastMetrics() {
  const lastMetrics = await prisma.metrics.findFirst({
    orderBy: {
      id: 'desc' // assuming 'id' is the primary key and auto-incrementing
    }
  });

  return lastMetrics;
}

getLastMetrics()
  .then(lastMetrics => {
    console.log('Last Metrics:', lastMetrics);
  })
  .catch(error => {
    console.error('Error retrieving last Metrics:', error);
  })
  .finally(async () => {
    await prisma.$disconnect();
  });