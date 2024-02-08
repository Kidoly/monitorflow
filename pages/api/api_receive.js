import { PrismaClient } from '@prisma/client';

const prisma = new PrismaClient();

export const config = {
  api: {
    bodyParser: {
      sizeLimit: '10mb',
    },
  },
};

export default async function handler(req, res) {
  if (req.method !== 'POST') {
    res.setHeader('Allow', ['POST']);
    res.status(405).end(`Method ${req.method} Not Allowed`);
    return;
  }

  const { password, ...data } = req.body;

  const expectedPassword = "EpsiEpsi2024";

  // Verify the password
  if (password !== expectedPassword) {
    // If the password is incorrect, return an error response
    res.status(401).json({ error: "Unauthorized", message: "Incorrect password" });
    return;
  }

  try {
    // Proceed with your database operation if the password is correct
    const result = await prisma.metrics.create({
      data: {
        startTime: new Date(data.start_time * 1000),
        totalMemory: data.total_memory,
        usedMemory: data.used_memory,
        totalSwap: data.total_swap,
        usedSwap: data.used_swap,
        systemName: data.system_name,
        kernelVersion: data.kernel_version,
        osVersion: data.os_version,
        hostName: data.host_name,
        cpuCount: data.cpu_count,
        cpuName: data.cpu_name,
        disksNumbers: data.disks_numbers,
        disks: JSON.stringify(data.disks),
        networks: JSON.stringify(data.networks),
        components: data.components ? Buffer.from(JSON.stringify(data.components)) : null,
        processesCount: data.processes_count,
        processes: data.processes ? Buffer.from(JSON.stringify(data.processes)) : null,
        monitor: data.monitor ? Buffer.from(JSON.stringify(data.monitor)) : null,
      },
    });

    res.status(200).json({ message: 'Data inserted successfully', result });
  } catch (error) {
    console.error('Request error', error);
    res.status(500).json({ error: 'Error inserting data', message: error.message });
  } finally {
    await prisma.$disconnect();
  }
}