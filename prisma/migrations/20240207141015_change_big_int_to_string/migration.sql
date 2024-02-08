/*
  Warnings:

  - Made the column `totalMemory` on table `metrics` required. This step will fail if there are existing NULL values in that column.

*/
-- AlterTable
ALTER TABLE `metrics` MODIFY `totalMemory` VARCHAR(191) NOT NULL,
    MODIFY `usedMemory` VARCHAR(191) NOT NULL,
    MODIFY `totalSwap` VARCHAR(191) NOT NULL,
    MODIFY `usedSwap` VARCHAR(191) NOT NULL;
