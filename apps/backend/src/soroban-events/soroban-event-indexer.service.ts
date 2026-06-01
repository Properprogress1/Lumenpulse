import { Injectable, Logger } from '@nestjs/common';
import { Cron, CronExpression } from '@nestjs/schedule';
import { InjectRepository } from '@nestjs/typeorm';
import { Repository } from 'typeorm';
import { ConfigService } from '@nestjs/config';
import { rpc } from '@stellar/stellar-sdk';
import { SorobanRpcClientService } from '../stellar/services/soroban-rpc-client.service';
import { JobLockService } from '../scheduler/job-lock.service';
import { JobHistoryService } from '../scheduler/job-history.service';
import { SorobanEvent, SorobanEventStatus } from './entities/soroban-event.entity';
import { SorobanIndexerCursor } from './entities/soroban-indexer-cursor.entity';

const JOB_NAME = 'soroban-event-indexer';
const GLOBAL_CURSOR_KEY = '__global__';

/** Max ledgers to scan per cron tick to avoid long-running queries */
const MAX_LEDGER_RANGE_PER_RUN = 1000;

/** Soroban RPC getEvents page size limit */
const PAGE_LIMIT = 100;

@Injectable()
export class SorobanEventIndexerService {
  private readonly logger = new Logger(SorobanEventIndexerService.name);

  constructor(
    private readonly rpcClient: SorobanRpcClientService,
    private readonly jobLock: JobLockService,
    private readonly jobHistory: JobHistoryService,
    private readonly configService: ConfigService,
    @InjectRepository(SorobanEvent)
    private readonly eventRepo: Repository<SorobanEvent>,
    @InjectRepository(SorobanIndexerCursor)
    private readonly cursorRepo: Repository<SorobanIndexerCursor>,
  ) {}

  /**
   * Incremental sync — runs every 30 seconds.
   * Picks up from the last indexed ledger and walks forward to the latest.
   */
  @Cron(CronExpression.EVERY_30_SECONDS)
  async runIncrementalSync(): Promise<void> {
    await this.jobLock.withLock(JOB_NAME, () => this.sync('scheduled'));
  }

  /**
   * Backfill from a specific start ledger.
   * Call this manually (e.g. via a one-off script or admin endpoint) to
   * re-index historical data.
   */
  async backfill(fromLedger: number): Promise<{ indexed: number }> {
    this.logger.log(`Starting backfill from ledger ${fromLedger}`);
    return this.sync('backfill', fromLedger);
  }

  // ---------------------------------------------------------------------------
  // Internal
  // ---------------------------------------------------------------------------

  private async sync(
    triggeredBy: string,
    overrideStartLedger?: number,
  ): Promise<{ indexed: number }> {
    const run = await this.jobHistory.start(JOB_NAME, triggeredBy);

    try {
      const latestLedger = await this.fetchLatestLedger();
      if (latestLedger === null) {
        await this.jobHistory.complete(run, { indexed: 0, reason: 'rpc-unavailable' });
        return { indexed: 0 };
      }

      const cursor = await this.getOrCreateCursor(GLOBAL_CURSOR_KEY);
      const startLedger = overrideStartLedger ?? cursor.lastLedgerSequence + 1;

      if (startLedger > latestLedger) {
        this.logger.debug(
          `Indexer up-to-date (cursor=${cursor.lastLedgerSequence}, latest=${latestLedger})`,
        );
        await this.jobHistory.complete(run, { indexed: 0, upToDate: true });
        return { indexed: 0 };
      }

      const endLedger = Math.min(
        startLedger + MAX_LEDGER_RANGE_PER_RUN - 1,
        latestLedger,
      );

      this.logger.log(
        `Indexing ledgers ${startLedger}–${endLedger} (latest=${latestLedger})`,
      );

      const indexed = await this.indexLedgerRange(startLedger, endLedger);

      // Advance cursor only after successful indexing
      await this.cursorRepo.save({
        cursorKey: GLOBAL_CURSOR_KEY,
        lastLedgerSequence: endLedger,
      });

      await this.jobHistory.complete(run, {
        indexed,
        startLedger,
        endLedger,
      });

      this.logger.log(
        `Indexed ${indexed} events for ledgers ${startLedger}–${endLedger}`,
      );

      return { indexed };
    } catch (err) {
      await this.jobHistory.fail(run, err);
      this.logger.error('Soroban event indexer failed', err);
      return { indexed: 0 };
    }
  }

  /**
   * Fetch all Soroban events in [startLedger, endLedger] using pagination,
   * then upsert them idempotently.
   */
  private async indexLedgerRange(
    startLedger: number,
    endLedger: number,
  ): Promise<number> {
    const server = this.rpcClient.rawServer;
    let indexed = 0;
    let cursor: string | undefined;

    do {
      const request: rpc.Server.GetEventsRequest = {
        startLedger,
        filters: [],
        limit: PAGE_LIMIT,
        ...(cursor ? { cursor } : {}),
      };

      const response = await server.getEvents(request);

      if (!response.events || response.events.length === 0) {
        break;
      }

      // Filter to events within our target range
      const eventsInRange = response.events.filter((e) => {
        const seq = Number(e.ledger);
        return seq >= startLedger && seq <= endLedger;
      });

      await this.upsertEvents(eventsInRange);
      indexed += eventsInRange.length;

      // Advance pagination cursor
      const lastEvent = response.events[response.events.length - 1];
      cursor = lastEvent?.id;

      // Stop if the last event is beyond our range or no more pages
      const lastLedger = Number(lastEvent?.ledger ?? 0);
      if (lastLedger > endLedger || response.events.length < PAGE_LIMIT) {
        break;
      }
    } while (true);

    return indexed;
  }

  /**
   * Upsert a batch of raw RPC events into the soroban_events table.
   * Uses (txHash, eventIndex) as the idempotency key.
   */
  private async upsertEvents(
    events: rpc.Api.RawEventResponse[],
  ): Promise<void> {
    if (events.length === 0) return;

    const rows = events.map((e) => {
      const txHash = e.txHash ?? '';
      // eventIndex is the numeric part after the last dash in the event id
      // e.g. "0000000012345678-0000000001" → index 1
      const eventIndex = this.parseEventIndex(e.id);
      const contractId = e.contractId ?? null;
      const eventType = this.extractEventType(e);
      const ledgerSequence = Number(e.ledger);

      return this.eventRepo.create({
        txHash,
        eventIndex,
        contractId,
        eventType,
        ledgerSequence,
        rawPayload: {
          id: e.id,
          type: e.type,
          ledger: e.ledger,
          ledgerClosedAt: e.ledgerClosedAt,
          pagingToken: e.pagingToken,
          topic: e.topic,
          value: e.value,
          inSuccessfulContractCall: e.inSuccessfulContractCall,
        } as Record<string, unknown>,
        status: SorobanEventStatus.PENDING,
        errorMessage: null,
        processedAt: null,
      });
    });

    // Upsert — on conflict (txHash, eventIndex) do nothing (idempotent)
    await this.eventRepo
      .createQueryBuilder()
      .insert()
      .into(SorobanEvent)
      .values(rows)
      .orIgnore()
      .execute();
  }

  private async fetchLatestLedger(): Promise<number | null> {
    try {
      const server = this.rpcClient.rawServer;
      const latest = await server.getLatestLedger();
      return latest.sequence;
    } catch (err) {
      this.logger.warn('Failed to fetch latest ledger from RPC', err);
      return null;
    }
  }

  private async getOrCreateCursor(key: string): Promise<SorobanIndexerCursor> {
    const existing = await this.cursorRepo.findOne({
      where: { cursorKey: key },
    });
    if (existing) return existing;

    // Bootstrap: start from the configured backfill ledger or 0
    const bootstrapLedger = this.configService.get<number>(
      'SOROBAN_INDEXER_START_LEDGER',
      0,
    );

    const cursor = this.cursorRepo.create({
      cursorKey: key,
      lastLedgerSequence: bootstrapLedger,
    });
    return this.cursorRepo.save(cursor);
  }

  /** Parse the numeric event index from a Soroban event ID string. */
  private parseEventIndex(eventId: string): number {
    if (!eventId) return 0;
    const parts = eventId.split('-');
    const last = parts[parts.length - 1];
    const parsed = parseInt(last, 10);
    return Number.isNaN(parsed) ? 0 : parsed;
  }

  /** Extract a human-readable event type from the topic array. */
  private extractEventType(e: rpc.Api.RawEventResponse): string | null {
    try {
      const topics = e.topic;
      if (!topics || topics.length === 0) return null;
      // First topic is typically the event name as a Symbol SCVal
      const first = topics[0];
      if (typeof first === 'string') return first;
      // If it's an object with a sym field (XDR decoded)
      if (typeof first === 'object' && first !== null) {
        const obj = first as Record<string, unknown>;
        if (typeof obj['sym'] === 'string') return obj['sym'];
        if (typeof obj['str'] === 'string') return obj['str'];
      }
      return null;
    } catch {
      return null;
    }
  }
}
